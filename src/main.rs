//! Main application entrypoint for pure-Rust Memento with Slint UI and live MPV backend.

use std::cell::{Cell, RefCell};
use std::process::Command;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use glow::HasContext;
use slint::{ComponentHandle, Model as _, Rgba8Pixel, SharedPixelBuffer, VecModel};

pub mod ui;
pub mod controller;
mod anki_help;
mod settings_ui;
mod video_gl;
mod graphics_device;

pub use ui::{
    ChapterMark, DictResult, InstalledDictionaryRow, MementoApp, MementoPalette, PlayerTrack,
    PitchGraph, PitchMora, PopupAudioSource, PopupBridge, PopupFrequency, PopupGlossaryBlock,
    AnkiHelpWindow, SettingsWindow, SubEntry, SubtitleLine, SubtitleOverlayBridge, SubtitleWord,
};
use ui::{publish_active_subtitle, dictionary_result_rows, set_dictionary_rows, popup_pitch_graphs, subtitle_lines_for_app};
#[cfg(test)]
use ui::{subtitle_lines_from_words, subtitle_token_width};
use settings_ui::{
    create_settings_session, refresh_category_settings, refresh_custom_popup_css,
    refresh_dictionary_settings, SettingsSession,
};
#[cfg(test)]
use settings_ui::clear_asr_for_settings;
use controller::anki::AnkiController;
use controller::config::ConfigController;
use controller::dict::DictController;
use controller::player::{
    clear_primary_subtitle, compute_autofit_window_size, compute_video_render_size,
    effective_media_title, filter_subtitle_text, mpv_cursor_should_hide,
    refresh_subtitle_list as base_refresh_subtitle_list, refresh_subtitle_marks,
    subtitle_cursor_zones, LivePlayerCoordinator, MouseInputForwarder,
};
#[cfg(test)]
use controller::player::{
    clear_subtitle_marks_on_advance, sync_player_tracks as sync_player_tracks_with_asr,
};
use controller::subtitles::{SubtitleController, ExternalSubtitleLoad};
#[cfg(test)]
use controller::subtitles::{MemorySubtitleCue, ASR_TRACK_ID};
use controller::dictmanager::DictionaryManager;
use memento_config::{ArgbColor, QtFontSpec, Value};
use memento_player::{MpvInstance, PlaylistLoad};
use memento_setting::keys as raw;

struct AppLogger;
impl log::Log for AppLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        !metadata.target().starts_with("icu_")
    }
    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            eprintln!("[{}] {}", record.level(), record.args());
        }
    }
    fn flush(&self) {}
}
static APP_LOGGER: AppLogger = AppLogger;

/// Match Qt's Linux AppConfigLocation/AppDataLocation used by the C++ app.
fn memento_xdg_dir(variable: &str, home_suffix: &str) -> Option<PathBuf> {
    std::env::var_os(variable)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(home_suffix)))
        .map(|root| root.join("memento"))
}

/// Migrate a legacy config file from `legacy_dir` (`~/.cache/memento`) into
/// `config_dir` (`~/.config/memento`) if the destination does not exist yet.
fn migrate_xdg_config_if_needed(
    legacy_dir: &Path,
    config_dir: &Path,
    file_name: &str,
) -> std::io::Result<bool> {
    let legacy_path = legacy_dir.join(file_name);
    let target_path = config_dir.join(file_name);
    if legacy_path == target_path || target_path.exists() || !legacy_path.is_file() {
        return Ok(false);
    }
    std::fs::create_dir_all(config_dir)?;
    let mut source = std::fs::File::open(&legacy_path)?;
    let mut temp = tempfile::NamedTempFile::new_in(config_dir)?;
    std::io::copy(&mut source, temp.as_file_mut())?;
    temp.as_file().sync_all()?;
    match temp.persist_noclobber(&target_path) {
        Ok(_) => Ok(true),
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error.error),
    }
}

fn format_time(sec: f64) -> String {
    let total = sec.max(0.0) as i64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 {
        format!("{:02}:{:02}:{:02}", h, m, s)
    } else {
        format!("{:02}:{:02}", m, s)
    }
}

const MEMENTO_RELEASES_URL: &str = "https://github.com/ripose-jp/Memento/releases/latest";

/// Query the same release endpoint as the original Memento update dialog.
fn latest_release() -> Result<String, String> {
    use memento_net::http::{HttpClient, Request, Url};
    let url = Url::parse("https://api.github.com/repos/ripose-jp/memento/releases/latest")
        .map_err(|error| error.to_string())?;
    let request = Request::get(url)
        .header("User-Agent", format!("Memento/{}", env!("CARGO_PKG_VERSION")))
        .header("Accept", "application/vnd.github+json");
    let mut response = HttpClient::default()
        .send(&request)
        .map_err(|error| error.to_string())?;
    if response.status() != 200 {
        return Err(format!("GitHub returned HTTP {}", response.status()));
    }
    let body = response.read_all(256 * 1024).map_err(|error| error.to_string())?;
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

pub(crate) fn open_update_opt_in_dialog(app: &MementoApp) {
    app.set_update_dialog_opt_in(true);
    app.set_update_dialog_title("Auto Update".into());
    app.set_update_dialog_message(
        "Do you want to check for updates on launch?\n\nThis can be changed at any time in Settings → Options → Application.\nThis will query GitHub's releases API when Memento starts."
            .into(),
    );
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

fn spawn_update_check(response_app: slint::Weak<MementoApp>, silent: bool) {
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

/// Decode subtitle file bytes supporting UTF-8 (with or without BOM),
/// UTF-16 LE/BE (with or without BOM, including CJK leading characters), and
/// lossy UTF-8 fallback.
fn decode_subtitle_bytes(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(b"\xef\xbb\xbf") {
        return String::from_utf8_lossy(rest).into_owned();
    }
    if let Some(rest) = bytes.strip_prefix(b"\xff\xfe") {
        let u16s: Vec<u16> = rest
            .chunks_exact(2)
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
            .collect();
        return String::from_utf16_lossy(&u16s);
    }
    if let Some(rest) = bytes.strip_prefix(b"\xfe\xff") {
        let u16s: Vec<u16> = rest
            .chunks_exact(2)
            .map(|chunk| u16::from_be_bytes([chunk[0], chunk[1]]))
            .collect();
        return String::from_utf16_lossy(&u16s);
    }
    if !bytes.contains(&0) {
        if let Ok(utf8) = std::str::from_utf8(bytes) {
            return utf8.to_owned();
        }
    }
    if bytes.len() >= 2 && bytes.len() % 2 == 0 {
        let sample_len = bytes.len().min(4096) & !1;
        let sample = &bytes[..sample_len];
        let total_pairs = sample.len() / 2;
        let mut le_newlines = 0usize;
        let mut be_newlines = 0usize;
        let mut le_ascii = 0usize;
        let mut be_ascii = 0usize;
        for chunk in sample.chunks_exact(2) {
            if chunk[1] == 0 {
                if matches!(chunk[0], b'\n' | b'\r') {
                    le_newlines += 1;
                }
                if matches!(chunk[0], b'\t' | b'\n' | b'\r' | 0x20..=0x7e) {
                    le_ascii += 1;
                }
            }
            if chunk[0] == 0 {
                if matches!(chunk[1], b'\n' | b'\r') {
                    be_newlines += 1;
                }
                if matches!(chunk[1], b'\t' | b'\n' | b'\r' | 0x20..=0x7e) {
                    be_ascii += 1;
                }
            }
        }
        let looks_like_utf16 = le_newlines > 0
            || be_newlines > 0
            || ((le_ascii + be_ascii) * 4 >= total_pairs && (le_ascii >= 2 || be_ascii >= 2));
        if looks_like_utf16 {
            let u16s: Vec<u16> = if (le_newlines, le_ascii) >= (be_newlines, be_ascii) {
                bytes
                    .chunks_exact(2)
                    .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
                    .collect()
            } else {
                bytes
                    .chunks_exact(2)
                    .map(|chunk| u16::from_be_bytes([chunk[0], chunk[1]]))
                    .collect()
            };
            return String::from_utf16_lossy(&u16s);
        }
    }
    String::from_utf8_lossy(bytes).into_owned()
}

/// Read a subtitle file supporting UTF-8 (with or without BOM), UTF-16 LE/BE, and lossy UTF-8.
fn read_subtitle_file(path: &Path) -> Result<String, String> {
    let bytes =
        std::fs::read(path).map_err(|error| format!("Could not read {}: {error}", path.display()))?;
    Ok(decode_subtitle_bytes(&bytes))
}

fn parse_lenient_timecode(s: &str) -> Option<f64> {
    let s = s.trim();
    let (h_m_s, frac) = match s.split_once(['.', ',']) {
        Some((front, back)) => {
            let digits: String = back.chars().take_while(|c| c.is_ascii_digit()).collect();
            let val = if digits.is_empty() {
                0.0
            } else {
                digits.parse::<f64>().ok()? / 10f64.powi(digits.len() as i32)
            };
            (front, val)
        }
        None => (s, 0.0),
    };
    let parts: Vec<&str> = h_m_s.split(':').collect();
    match parts.len() {
        3 => {
            let h = parts[0].trim().parse::<f64>().ok()?;
            let m = parts[1].trim().parse::<f64>().ok()?;
            let sec = parts[2].trim().parse::<f64>().ok()?;
            Some(h * 3600.0 + m * 60.0 + sec + frac)
        }
        2 => {
            let m = parts[0].trim().parse::<f64>().ok()?;
            let sec = parts[1].trim().parse::<f64>().ok()?;
            Some(m * 60.0 + sec + frac)
        }
        1 => {
            let sec = parts[0].trim().parse::<f64>().ok()?;
            Some(sec + frac)
        }
        _ => None,
    }
}

fn strip_ass_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_tag = false;
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '{' {
            in_tag = true;
        } else if chars[i] == '}' {
            in_tag = false;
        } else if !in_tag {
            if chars[i] == '\\' && i + 1 < chars.len() && (chars[i + 1] == 'N' || chars[i + 1] == 'n') {
                out.push('\n');
                i += 1;
            } else {
                out.push(chars[i]);
            }
        }
        i += 1;
    }
    out
}

fn parse_lenient_subtitles(text: &str) -> Vec<memento_subtitle::SubtitleEntry> {
    let mut entries = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.to_ascii_lowercase().starts_with("dialogue:") {
            let rest = if let Some(r) = trimmed.strip_prefix("Dialogue: ") {
                r
            } else if let Some(r) = trimmed.strip_prefix("Dialogue:") {
                r.trim_start()
            } else {
                continue;
            };
            let columns: Vec<&str> = rest.split(',').collect();
            if columns.len() >= 3 {
                // Typical ASS Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text
                // Start is index 1, End is index 2, Text is index 9..
                if let (Some(start), Some(end)) = (
                    parse_lenient_timecode(columns[1]),
                    parse_lenient_timecode(columns[2]),
                ) {
                    if start >= 0.0 && end > start {
                        let text_joined = if columns.len() > 9 {
                            columns[9..].join(",")
                        } else if columns.len() > 3 {
                            columns[3..].join(",")
                        } else {
                            String::new()
                        };
                        let clean = strip_ass_tags(&text_joined);
                        let clean_trimmed = clean.trim();
                        if !clean_trimmed.is_empty() {
                            entries.push(memento_subtitle::SubtitleEntry {
                                text: clean_trimmed.encode_utf16().collect(),
                                start,
                                end,
                            });
                        }
                    }
                }
            }
        }
    }
    entries.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap_or(std::cmp::Ordering::Equal));
    entries
}

/// Read an external subtitle file reported by mpv for one specific SID.
/// Embedded tracks have no path; those are populated from mpv's live cue data.
fn parse_external_subtitles(path: &Path) -> Result<Vec<memento_subtitle::SubtitleEntry>, String> {
    let extension = path.extension().and_then(|extension| extension.to_str())
        .unwrap_or_default().to_ascii_lowercase();
    let text = read_subtitle_file(path)?;
    let u16_text: Vec<u16> = text.encode_utf16().collect();

    let primary_format = match extension.as_str() {
        "ass" | "ssa" => Some(memento_subtitle::Format::Ass),
        "srt" => Some(memento_subtitle::Format::Srt),
        "vtt" => Some(memento_subtitle::Format::Vtt),
        _ => None,
    };
    if let Some(format) = primary_format {
        if let Ok(cues) = memento_subtitle::parse_subtitles(format, &u16_text) {
            if !cues.is_empty() {
                return Ok(cues);
            }
        }
    }
    for format in [memento_subtitle::Format::Ass, memento_subtitle::Format::Srt, memento_subtitle::Format::Vtt] {
        if Some(format) == primary_format {
            continue;
        }
        if let Ok(cues) = memento_subtitle::parse_subtitles(format, &u16_text) {
            if !cues.is_empty() {
                return Ok(cues);
            }
        }
    }
    let fallback_cues = parse_lenient_subtitles(&text);
    if !fallback_cues.is_empty() {
        return Ok(fallback_cues);
    }
    Err(format!("Could not parse subtitle file: {}", path.display()))
}

#[allow(clippy::too_many_arguments)]
fn load_playlist_into(
    app: &MementoApp,
    player: &MpvInstance,
    loads: &[PlaylistLoad],
    media_generation: &Cell<u64>,
    shared_media_generation: &AtomicU64,
    coordinator: Option<&RefCell<Option<LivePlayerCoordinator>>>,
    keep_paused: bool,
) -> Result<Option<String>, String> {
    let mut subtitle_paths = Vec::new();
    let mut media_entries = Vec::new();
    for entry in loads {
        if entry.path.is_empty() {
            continue;
        }
        if controller::player::is_subtitle_file(entry.path.as_str()) {
            subtitle_paths.push(entry.path.clone());
        } else {
            media_entries.push(entry);
        }
    }
    if media_entries.is_empty() {
        for sub_path in &subtitle_paths {
            player.command(&["sub-add", sub_path])?;
        }
        return Ok(None);
    }
    let mut first_media_path: Option<String> = None;
    for entry in media_entries {
        if first_media_path.is_none() {
            player.pause()?;
            let mut opts = entry.options.clone();
            for sub_path in &subtitle_paths {
                opts.push(format!("sub-files-append={sub_path}"));
            }
            player.load_file_with_options(&entry.path, false, &opts)?;
            first_media_path = Some(entry.path.clone());
        } else {
            player.load_file_with_options(&entry.path, true, &entry.options)?;
        }
    }
    let Some(first_path) = first_media_path else {
        return Ok(None);
    };
    let next_gen = media_generation.get().wrapping_add(1);
    media_generation.set(next_gen);
    shared_media_generation.store(next_gen, Ordering::Release);
    if let Some(coord_cell) = coordinator {
        if let Ok(mut guard) = coord_cell.try_borrow_mut() {
            if let Some(coord) = guard.as_mut() {
                coord.mark_explicit_media_load(next_gen);
            }
        }
    }
    if keep_paused {
        app.set_pending_playback_generation(-1);
        app.set_is_playing(false);
    } else if player.is_opengl_active() {
        app.set_pending_playback_generation((next_gen & i32::MAX as u64) as i32);
        app.set_is_playing(true);
    } else {
        app.set_pending_playback_generation(-1);
        player.play()?;
        app.set_is_playing(true);
    }
    let title = effective_media_title("", &first_path);
    app.set_media_title(title.into());
    app.set_secondary_sub_text("".into());
    app.set_thumbnail_frame(slint::Image::default());
    app.set_hover_chapter_title("".into());
    clear_primary_subtitle(app);
    Ok(Some(first_path))
}

fn load_media_into(
    app: &MementoApp,
    player: &MpvInstance,
    path: &str,
    media_generation: &Cell<u64>,
    shared_media_generation: &AtomicU64,
    coordinator: Option<&RefCell<Option<LivePlayerCoordinator>>>,
) -> Result<(), String> {
    let single = [PlaylistLoad {
        path: path.to_owned(),
        options: Vec::new(),
    }];
    load_playlist_into(
        app,
        player,
        &single,
        media_generation,
        shared_media_generation,
        coordinator,
        false,
    )?;
    Ok(())
}

static RECENT_TITLES: std::sync::OnceLock<Mutex<std::collections::HashMap<String, String>>> =
    std::sync::OnceLock::new();

fn recent_titles_map() -> &'static Mutex<std::collections::HashMap<String, String>> {
    RECENT_TITLES.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

pub fn record_recent_media_title(path: &str, media_title: &str) {
    if path.trim().is_empty() {
        return;
    }
    let effective = effective_media_title(media_title, path);
    if !effective.is_empty() {
        if let Ok(mut map) = recent_titles_map().lock() {
            map.insert(path.to_owned(), effective);
        }
    }
}

pub fn format_recent_file_label(media_title: &str, path: &str) -> String {
    let title = effective_media_title(media_title, path);
    if title.is_empty() || title == path {
        path.to_owned()
    } else {
        format!("{title} — {path}")
    }
}

fn record_recent_media(
    app: &MementoApp,
    config: &ConfigController,
    cache_dir: &Path,
    path: &str,
) -> Result<(), String> {
    record_recent_media_title(path, app.get_media_title().as_str());
    config.add_recent_file(path)?;
    config.save(cache_dir, "memento.conf")?;
    refresh_recent_files(app, config);
    Ok(())
}

pub(crate) fn open_directory_command(path: &Path) -> Command {
    #[cfg(target_os = "macos")]
    let mut cmd = Command::new("open");
    #[cfg(target_os = "windows")]
    let mut cmd = Command::new("explorer");
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut cmd = Command::new("xdg-open");
    cmd.arg(path);
    cmd
}

pub(crate) fn spawn_and_reap(mut cmd: Command) -> std::io::Result<u32> {
    let mut child = cmd.spawn()?;
    let pid = child.id();
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(pid)
}

fn exact_matcher_enabled(config: &ConfigController) -> bool {
    setting_enabled(
        config,
        raw::search::GROUP,
        raw::search::matcher::EXACT,
        raw::search::matcher::EXACT_DEFAULT,
    )
}

fn deconj_matcher_enabled(config: &ConfigController) -> bool {
    setting_enabled(
        config,
        raw::search::GROUP,
        raw::search::matcher::DECONJ,
        raw::search::matcher::DECONJ_DEFAULT,
    )
}

fn mecab_matcher_enabled(config: &ConfigController) -> bool {
    match config.effective_setting(raw::search::GROUP, raw::search::matcher::MECAB_IPADIC) {
        Some(Value::Bool(b)) => b,
        _ => raw::search::matcher::MECAB_IPADIC_DEFAULT,
    }
}

fn search_text_filters(config: &ConfigController) -> (String, bool, String) {
    let remove_regex = setting_text(
        config,
        raw::search::GROUP,
        raw::search::REMOVE_REGEX,
        raw::search::REMOVE_REGEX_DEFAULT,
    );
    let replace_lines = setting_enabled(
        config,
        raw::search::GROUP,
        raw::search::REPLACE_NEWLINES,
        raw::search::REPLACE_NEWLINES_DEFAULT,
    );
    let replace_with = setting_text(
        config,
        raw::search::GROUP,
        raw::search::REPLACE_NEWLINES_WITH,
        raw::search::REPLACE_NEWLINES_WITH_DEFAULT,
    );
    (remove_regex, replace_lines, replace_with)
}

fn apply_dictionary_order(config: &ConfigController, dictionary: &mut DictController) {
    dictionary.set_exact_enabled(exact_matcher_enabled(config));
    dictionary.set_deconj_enabled(deconj_matcher_enabled(config));
    dictionary.set_mecab_enabled(mecab_matcher_enabled(config));
    let (remove_regex, replace_lines, replace_with) = search_text_filters(config);
    dictionary.set_search_text_filters(&remove_regex, replace_lines, &replace_with);
    if let Some(Value::Ints(order)) =
        config.effective_setting(raw::dictionary::GROUP, raw::dictionary::ORDER)
    {
        dictionary.set_order(&order);
    }
}

fn maybe_auto_play_first_term_audio(app: &MementoApp, expression: &str, reading: &str) {
    if app.get_auto_play_audio_enabled() && !expression.is_empty() {
        app.invoke_play_audio_for_word(expression.into(), reading.into());
    }
}

struct WordLookupRequest {
    generation: u64,
    query: String,
    order: Vec<i64>,
    source_subtitle: String,
    excluded: Vec<String>,
    exact_enabled: bool,
    deconj_enabled: bool,
    mecab_enabled: bool,
    remove_regex: String,
    replace_lines: bool,
    replace_with: String,
}

fn parse_xdg_user_dir(contents: &str, key: &str, home: &Path) -> Option<PathBuf> {
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((lhs, rhs)) = trimmed.split_once('=') else {
            continue;
        };
        if lhs.trim() != key {
            continue;
        }
        let val = rhs.trim().trim_matches('"').trim_matches('\'');
        if val.is_empty() {
            continue;
        }
        if let Some(rest) = val.strip_prefix("$HOME/") {
            return Some(home.join(rest));
        }
        if val == "$HOME" {
            return Some(home.to_path_buf());
        }
        return Some(PathBuf::from(val));
    }
    None
}

pub(crate) fn decode_custom_open_dir(custom: &str, home_dir: Option<&Path>) -> Option<PathBuf> {
    let trimmed = custom.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with("file://") {
        return memento_os::decode_file_uri(trimmed);
    }
    if let Some(rest) = trimmed.strip_prefix("~/") {
        return home_dir.map(|home| home.join(rest));
    }
    if trimmed == "~" {
        return home_dir.map(Path::to_path_buf);
    }
    Some(PathBuf::from(trimmed))
}

/// Resolve the starting directory for `FileDialogOptions` from `behavior/file-open-dir`
/// (`0=Current`, `1=Home`, `2=Movies`, `3=Documents`, `4=Custom`) and `behavior/file-open-custom`
/// with existence checks and graceful fallbacks.
pub(crate) fn resolve_file_open_dir(
    dir_kind: i64,
    custom: &str,
    home_dir: Option<&Path>,
    current_dir: Option<&Path>,
    videos_dir: Option<&Path>,
    documents_dir: Option<&Path>,
) -> Option<PathBuf> {
    let existing_dir = |candidate: Option<PathBuf>| candidate.filter(|p| p.is_dir());
    let home_fallback = || {
        existing_dir(home_dir.map(Path::to_path_buf))
            .or_else(|| existing_dir(current_dir.map(Path::to_path_buf)))
    };

    match dir_kind {
        0 => existing_dir(current_dir.map(Path::to_path_buf)).or_else(home_fallback),
        1 => home_fallback(),
        2 => existing_dir(videos_dir.map(Path::to_path_buf))
            .or_else(|| existing_dir(home_dir.map(|h| h.join("Videos"))))
            .or_else(|| existing_dir(home_dir.map(|h| h.join("Movies"))))
            .or_else(home_fallback),
        3 => existing_dir(documents_dir.map(Path::to_path_buf))
            .or_else(|| existing_dir(home_dir.map(|h| h.join("Documents"))))
            .or_else(home_fallback),
        4 => existing_dir(decode_custom_open_dir(custom, home_dir)).or_else(home_fallback),
        _ => existing_dir(videos_dir.map(Path::to_path_buf))
            .or_else(|| existing_dir(home_dir.map(|h| h.join("Videos"))))
            .or_else(|| existing_dir(home_dir.map(|h| h.join("Movies"))))
            .or_else(home_fallback),
    }
}

fn resolve_config_file_open_dir(config: &ConfigController) -> Option<PathBuf> {
    let dir_kind = match config.effective_setting(raw::behavior::GROUP, raw::behavior::FILE_OPEN_DIR)
    {
        Some(Value::Int(v)) => v,
        _ => raw::behavior::FILE_OPEN_DIR_DEFAULT as i64,
    };
    let custom = setting_text(config, raw::behavior::GROUP, raw::behavior::FILE_OPEN_CUSTOM, "");
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from);
    let current = std::env::current_dir().ok();
    let user_dirs_content = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|h| h.join(".config")))
        .and_then(|cfg_dir| std::fs::read_to_string(cfg_dir.join("user-dirs.dirs")).ok());
    let videos = std::env::var_os("XDG_VIDEOS_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            let home_ref = home.as_deref()?;
            user_dirs_content
                .as_deref()
                .and_then(|c| parse_xdg_user_dir(c, "XDG_VIDEOS_DIR", home_ref))
        });
    let documents = std::env::var_os("XDG_DOCUMENTS_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            let home_ref = home.as_deref()?;
            user_dirs_content
                .as_deref()
                .and_then(|c| parse_xdg_user_dir(c, "XDG_DOCUMENTS_DIR", home_ref))
        });
    resolve_file_open_dir(
        dir_kind,
        &custom,
        home.as_deref(),
        current.as_deref(),
        videos.as_deref(),
        documents.as_deref(),
    )
}

fn set_popup_term_metadata(
    app: &MementoApp, expression: &str, reading: &str,
    graphs: Vec<controller::dict::TermPitchGraph>,
    frequencies: Vec<controller::dict::TermFrequency>,
) {
    let rows = app.get_dict_results();
    if let Some(index) = (0..rows.row_count()).find(|&index| rows.row_data(index)
        .is_some_and(|row| row.term_start && row.expression.as_str() == expression && row.reading.as_str() == reading)) {
        if let Some(mut row) = rows.row_data(index) {
            row.pitch_graphs = Rc::new(VecModel::from(popup_pitch_graphs(graphs))).into();
            row.frequencies = Rc::new(VecModel::from(frequencies.into_iter().map(|entry| PopupFrequency {
                dictionary: entry.dictionary.into(), value: entry.value.into(),
            }).collect::<Vec<_>>())).into();
            rows.set_row_data(index, row.clone());
            let popup = app.get_popup_results();
            if let Some(popup_index) = (0..popup.row_count()).find(|&popup_index| popup.row_data(popup_index)
                .is_some_and(|visible| visible.index == row.index)) {
                popup.set_row_data(popup_index, row);
            }
        }
    }
}

#[derive(Default)]
struct PopupAudioState {
    serial: u64,
    expression: String,
    reading: String,
    sources: Vec<controller::config::AudioSourceEntry>,
}

fn set_popup_audio_sources(
    app: &MementoApp,
    cache: &Arc<Mutex<PopupAudioState>>,
    expression: &str,
    reading: &str,
    entries: Vec<controller::config::AudioSourceEntry>,
) {
    let sources: Vec<PopupAudioSource> = entries.iter().enumerate()
        .map(|(index, source)| PopupAudioSource { name: source.name.clone().into(), index: index as i32 })
        .collect();
    {
        let mut state = cache.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        *state = PopupAudioState {
            serial: state.serial.wrapping_add(1),
            expression: expression.to_owned(), reading: reading.to_owned(), sources: entries,
        };
    }
    app.set_popup_audio_sources(Rc::new(VecModel::from(sources)).into());
    app.set_popup_selected_audio_source(-1);
    app.set_popup_audio_sources_loading(false);
}

struct WordLookupResult {
    hits: Vec<controller::dict::SearchHit>,
    error: Option<String>,
    database_open: bool,
    matched_units: usize,
    headword: String,
    reading: String,
    pitch: String,
    freq: String,
    jpdb: String,
}

struct VideoRenderWorker {
    stop: std::sync::mpsc::SyncSender<(u32, u32)>,
    thread: Option<std::thread::JoinHandle<()>>,
}

#[derive(Default)]
struct VideoTimings {
    paint_started: Option<std::time::Instant>,
    paint_popup_rows: usize,
    slowest_paint_popup_rows: usize,
    paint_max: std::time::Duration,
    video_max: std::time::Duration,
    queue_max: std::time::Duration,
    wake_max: std::time::Duration,
    sync_max: std::time::Duration,
    sync_gap_max: std::time::Duration,
    present_max: std::time::Duration,
    frames: u32,
}

impl VideoTimings {
    fn finish_paint(&mut self) {
        if let Some(started) = self.paint_started.take() {
            let elapsed = started.elapsed();
            if elapsed > self.paint_max {
                self.paint_max = elapsed;
                self.slowest_paint_popup_rows = self.paint_popup_rows;
            }
        }
    }

    fn report(&mut self, state: &memento_player::mpv::PlaybackState, drops_changed: bool, verbose: bool, pending: std::time::Duration) {
        if (verbose && self.frames > 0) || drops_changed || self.paint_max.as_millis() > 40 || self.queue_max.as_millis() > 40 {
            eprintln!(
                "[video] frames={} drops={} decoder-drops={} hwdec={} paused={} queue-max={:.1}ms mpv-max={:.1}ms draw-max={:.1}ms draw-popup-rows={} wake-max={:.1}ms sync-max={:.1}ms sync-gap={:.1}ms pending={:.1}ms present-max={:.1}ms",
                self.frames, state.frame_drops, state.decoder_drops,
                if state.hwdec_current.is_empty() { "no" } else { &state.hwdec_current },
                state.paused,
                self.queue_max.as_secs_f64() * 1000.0,
                self.video_max.as_secs_f64() * 1000.0,
                self.paint_max.as_secs_f64() * 1000.0,
                self.slowest_paint_popup_rows,
                self.wake_max.as_secs_f64() * 1000.0,
                self.sync_max.as_secs_f64() * 1000.0,
                self.sync_gap_max.as_secs_f64() * 1000.0,
                pending.as_secs_f64() * 1000.0,
                self.present_max.as_secs_f64() * 1000.0,
            );
        }
        *self = Self::default();
    }
}

// Includes time spent updating/layouting properties before BeforeRendering,
// which the renderer's draw timing cannot see. Drop also covers early returns.
struct SyncTiming {
    started: std::time::Instant,
    timings: Rc<RefCell<VideoTimings>>,
}

impl Drop for SyncTiming {
    fn drop(&mut self) {
        let elapsed = self.started.elapsed();
        let mut timings = self.timings.borrow_mut();
        timings.sync_max = timings.sync_max.max(elapsed);
    }
}

struct GlVideoSurface {
    gl: glow::Context,
    texture: Option<glow::NativeTexture>,
    framebuffer: Option<glow::NativeFramebuffer>,
    internal_format: i32,
    size: (u32, u32),
}

impl GlVideoSurface {
    fn new(resolver: &dyn Fn(&std::ffi::CStr) -> *const std::ffi::c_void, player: &MpvInstance) -> Self {
        let gl = unsafe { glow::Context::from_loader_function_cstr(resolver) };
        let renderer = unsafe { gl.get_parameter_string(glow::RENDERER) };
        let version = unsafe { gl.get_parameter_string(glow::VERSION) };
        eprintln!("OpenGL video device: {renderer}; {version}");
        if let Err(error) = player.configure_opengl_decoder(&renderer) {
            eprintln!("Could not configure video decoder interop: {error}");
        }
        Self {
            gl,
            texture: None,
            framebuffer: None,
            internal_format: glow::RGBA8 as i32,
            size: (0, 0),
        }
    }

    fn should_reallocate(&self, requested: (u32, u32)) -> bool {
        if self.texture.is_none() || self.framebuffer.is_none() || self.size == (0, 0) {
            return true;
        }
        if self.size == requested {
            return false;
        }
        if requested.0 > self.size.0 || requested.1 > self.size.1 {
            return true;
        }
        let aspect_diff = (u64::from(requested.0) * u64::from(self.size.1))
            .abs_diff(u64::from(self.size.0) * u64::from(requested.1));
        if aspect_diff > u64::from(self.size.0.max(requested.0)) * 2 {
            return true;
        }
        requested.0 + 64 <= self.size.0 || requested.1 + 64 <= self.size.1
    }

    fn ensure_size(&mut self, size: (u32, u32)) -> Result<(), String> {
        if !self.should_reallocate(size) {
            return Ok(());
        }
        let target = video_gl::create_video_fbo_texture(
            &self.gl,
            self.texture,
            self.framebuffer,
            size.0,
            size.1,
        )?;
        self.texture = Some(target.texture);
        self.framebuffer = Some(target.framebuffer);
        self.internal_format = target.internal_format;
        self.size = size;
        Ok(())
    }

    fn texture_info(&self) -> Option<(std::num::NonZeroU32, u32, u32)> {
        self.texture.map(|texture| (texture.0, self.size.0, self.size.1))
    }

    fn update(&self, player: &MpvInstance) -> bool {
        if !player.has_render_update() { return false; }
        video_gl::with_saved_state(&self.gl, || player.take_render_update())
    }

    fn render(&self, player: &MpvInstance) -> Result<(), String> {
        let Some(framebuffer) = self.framebuffer else { return Ok(()) };
        video_gl::with_saved_state(&self.gl, || {
            player.render_opengl_with_format(
                framebuffer.0.get(),
                self.size.0,
                self.size.1,
                self.internal_format,
            )
        })
    }

    fn destroy(mut self) {
        unsafe {
            if let Some(framebuffer) = self.framebuffer.take() { self.gl.delete_framebuffer(framebuffer); }
            if let Some(texture) = self.texture.take() { self.gl.delete_texture(texture); }
        }
    }
}

fn native_window_display_pointers(
    window: &slint::Window,
) -> (Option<*mut std::ffi::c_void>, Option<*mut std::ffi::c_void>) {
    use raw_window_handle::{HasDisplayHandle, RawDisplayHandle};
    let handle = window.window_handle();
    let Ok(display) = handle.display_handle() else {
        return (None, None);
    };
    match display.as_raw() {
        RawDisplayHandle::Xlib(xlib) => (xlib.display.map(|p| p.as_ptr()), None),
        RawDisplayHandle::Wayland(wl) => (None, Some(wl.display.as_ptr())),
        _ => (None, None),
    }
}

#[cfg(target_os = "linux")]
fn build_owned_gl_proc_resolver(
    scoped: &dyn Fn(&std::ffi::CStr) -> *const std::ffi::c_void,
    prefer_wayland_egl: bool,
) -> Option<impl Fn(&std::ffi::CStr) -> *const std::ffi::c_void + Send + Sync + 'static> {
    type GetProcFn = unsafe extern "C" fn(*const std::ffi::c_char) -> *const std::ffi::c_void;
    type GetCtxFn = unsafe extern "C" fn() -> *mut std::ffi::c_void;

    unsafe fn noload_sym(
        libs: &[&std::ffi::CStr],
        sym: &std::ffi::CStr,
    ) -> *const std::ffi::c_void {
        for lib in libs {
            let handle =
                unsafe { libc::dlopen(lib.as_ptr(), libc::RTLD_LAZY | libc::RTLD_NOLOAD) };
            if !handle.is_null() {
                let ptr = unsafe { libc::dlsym(handle, sym.as_ptr()) };
                unsafe { libc::dlclose(handle) };
                if !ptr.is_null() {
                    return ptr;
                }
            }
        }
        std::ptr::null()
    }

    let lookup_meta = |sym: &std::ffi::CStr, libs: &[&std::ffi::CStr]| -> *const std::ffi::c_void {
        let direct = scoped(sym);
        if !direct.is_null() {
            direct
        } else {
            unsafe { noload_sym(libs, sym) }
        }
    };

    let egl_libs: &[&std::ffi::CStr] = &[c"libEGL.so.1", c"libEGL.so"];
    let glx_libs: &[&std::ffi::CStr] = &[c"libGLX.so.0", c"libGL.so.1", c"libGLX.so"];

    let try_egl = || -> Option<GetProcFn> {
        let ctx_fn_ptr = lookup_meta(c"eglGetCurrentContext", egl_libs);
        let proc_fn_ptr = lookup_meta(c"eglGetProcAddress", egl_libs);
        if proc_fn_ptr.is_null() {
            return None;
        }
        if !ctx_fn_ptr.is_null() {
            let get_ctx: GetCtxFn = unsafe { std::mem::transmute(ctx_fn_ptr) };
            if unsafe { get_ctx() }.is_null() && !prefer_wayland_egl {
                return None;
            }
        } else if !prefer_wayland_egl {
            return None;
        }
        Some(unsafe { std::mem::transmute(proc_fn_ptr) })
    };

    let try_glx = || -> Option<GetProcFn> {
        let ctx_fn_ptr = lookup_meta(c"glXGetCurrentContext", glx_libs);
        let mut proc_fn_ptr = lookup_meta(c"glXGetProcAddressARB", glx_libs);
        if proc_fn_ptr.is_null() {
            proc_fn_ptr = lookup_meta(c"glXGetProcAddress", glx_libs);
        }
        if proc_fn_ptr.is_null() {
            return None;
        }
        if !ctx_fn_ptr.is_null() {
            let get_ctx: GetCtxFn = unsafe { std::mem::transmute(ctx_fn_ptr) };
            if unsafe { get_ctx() }.is_null() {
                return None;
            }
        }
        Some(unsafe { std::mem::transmute(proc_fn_ptr) })
    };

    let loader = if prefer_wayland_egl {
        try_egl().or_else(try_glx)
    } else {
        try_glx().or_else(try_egl)
    }?;

    Some(move |name: &std::ffi::CStr| unsafe {
        let ptr = loader(name.as_ptr());
        if !ptr.is_null() {
            ptr
        } else {
            libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr())
        }
    })
}

fn video_render_size(app: &MementoApp, player: &MpvInstance, opengl_active: bool) -> (u32, u32) {
    compute_video_render_size(
        app.get_video_width(),
        app.get_video_height(),
        app.window().scale_factor(),
        player.cached_video_dimensions(),
        opengl_active,
    )
}

impl Drop for VideoRenderWorker {
    fn drop(&mut self) {
        let _ = self.stop.send((0, 0));
        if let Some(thread) = self.thread.take() { let _ = thread.join(); }
    }
}

fn display_word_lookup(app: &MementoApp, result: WordLookupResult, query: &str, excluded: &[String]) {
    app.set_dict_open(false);
    app.set_popup_selected_audio_source(-1);
    app.set_anki_added_note_id("".into());
    app.set_anki_added_result_index(-1);
    if let Some(error) = result.error {
        app.set_status_message(format!("Dictionary search failed: {error}").into());
    } else if result.hits.is_empty() && !result.database_open && !query.trim().is_empty() {
        app.set_status_message("Dictionary database is unavailable".into());
    }
    set_dictionary_rows(app, dictionary_result_rows(&result.hits, excluded));
    app.invoke_reset_definition_scroll();
    app.set_dict_query(result.headword.clone().into());
    app.set_dict_reading(result.reading.into());
    app.set_dict_pitch_accent(result.pitch.into());
    app.set_dict_freq_rank(result.freq.into());
    app.set_dict_jpdb_rank(result.jpdb.into());
    app.set_dict_open(!result.hits.is_empty());

    let mut match_count = 1;
    if !result.headword.is_empty() {
        let start_idx = app.get_selected_token_index();
        if start_idx >= 0 {
            let sub_words = app.get_active_sub_words();
            let target_len = if result.matched_units > 0 {
                result.matched_units
            } else {
                result.headword.encode_utf16().count()
            };
            let mut accumulated = 0;
            let mut count = 0;
            for i in (start_idx as usize)..sub_words.row_count() {
                if let Some(word) = sub_words.row_data(i) {
                    accumulated += word.text.encode_utf16().count();
                    count += 1;
                    if accumulated >= target_len { break; }
                } else { break; }
            }
            if count > 0 { match_count = count as i32; }
        }
    }
    app.set_selected_token_count(match_count);
    if let Some(first_term) = result.hits.iter().find(|hit| !hit.is_kanji) {
        maybe_auto_play_first_term_audio(app, &first_term.expression, &first_term.reading);
    }
}


fn estimate_drawer_row_height(text: &str, available_text_width: f32, font_size: f32) -> f32 {
    let usable_w = available_text_width.max(120.0);
    let scale = (font_size.clamp(9.0, 48.0)) / 13.0;
    let mut visual_lines = 0usize;
    for raw_line in text.split('\n') {
        let line_px: f32 = raw_line
            .chars()
            .map(|ch| if ch.is_ascii() { 7.5 * scale } else { 13.5 * scale })
            .sum();
        let wrapped = ((line_px / usable_w).ceil() as usize).max(1);
        visual_lines += wrapped;
    }
    let visual_lines = visual_lines.max(1);
    let line_height = (18.0 * scale).max(16.0);
    (visual_lines as f32 * line_height + 14.0).max(36.0)
}

pub fn scroll_drawer_to_row_accurate(app: &MementoApp, row_idx: usize) {
    let is_secondary = app.get_subtitle_drawer_tab() == 1;
    let entries = if is_secondary {
        app.get_secondary_subtitle_entries()
    } else {
        app.get_subtitle_entries()
    };
    let count = entries.row_count();
    if count == 0 || row_idx >= count {
        return;
    }
    let chrome_w = if app.get_sub_list_show_timestamps() { 104.0 } else { 40.0 };
    let text_width = (app.get_sidebar_width() - chrome_w).max(120.0);
    let font_size = if is_secondary {
        app.get_sub_list_secondary_font_size()
    } else {
        app.get_sub_list_primary_font_size()
    };
    let mut row_y = 0.0f32;
    let mut target_h = 36.0f32;
    let mut total_h = 0.0f32;
    for idx in 0..count {
        let h = entries
            .row_data(idx)
            .map(|entry| estimate_drawer_row_height(entry.text.as_str(), text_width, font_size))
            .unwrap_or(36.0);
        if idx < row_idx {
            row_y += h;
        } else if idx == row_idx {
            target_h = h;
        }
        total_h += h;
    }
    app.invoke_scroll_subtitle_drawer_to_offset(row_y, target_h, total_h);
}

pub fn auto_scroll_subtitle_drawer_if_needed(
    app: &MementoApp,
    subtitles: &SubtitleController,
) {
    if !app.get_sub_drawer_open() || app.get_subtitle_drawer_user_scrolling() {
        return;
    }
    let active_idx = if app.get_subtitle_drawer_tab() == 1 {
        subtitles
            .active_indices(subtitles.secondary_sid())
            .first()
            .copied()
    } else {
        subtitles
            .active_indices(subtitles.primary_sid())
            .first()
            .copied()
    };
    if let Some(row) = active_idx {
        scroll_drawer_to_row_accurate(app, row);
    }
}

pub fn refresh_subtitle_list(
    app: &MementoApp,
    model: &VecModel<SubEntry>,
    subtitles: &SubtitleController,
) {
    base_refresh_subtitle_list(app, model, subtitles);
    auto_scroll_subtitle_drawer_if_needed(app, subtitles);
}

pub fn refresh_secondary_subtitle_list(
    app: &MementoApp,
    model: &VecModel<SubEntry>,
    subtitles: &SubtitleController,
) {
    let query = app.get_subtitle_search_query();
    let active_tab = app.get_subtitle_drawer_tab();
    let ignore_whitespace = app.get_subtitle_search_ignore_whitespace();
    let remove_regex = app.get_search_remove_regex();
    let compiled_regex = memento_dict::normalize::compile_remove_regex(remove_regex.as_str());
    let matching = if active_tab == 1 && !query.trim().is_empty() {
        search_entries(
            subtitles.secondary_entries(),
            &query,
            ignore_whitespace,
            compiled_regex.as_ref(),
        )
    } else {
        Vec::new()
    };
    let active = subtitles.active_indices(subtitles.secondary_sid());
    let rows: Vec<SubEntry> = subtitles
        .secondary_entries()
        .iter()
        .enumerate()
        .map(|(index, cue)| {
            let raw = String::from_utf16_lossy(&cue.text);
            let display = memento_dict::normalize::apply_compiled_remove_regex(
                &raw,
                compiled_regex.as_ref(),
            );
            SubEntry {
                id: (index + 1) as i32,
                start: cue.start as f32,
                end: cue.end as f32,
                text: display.into_owned().into(),
                selected: false,
                is_active: active.contains(&index),
                search_match: matching.contains(&index),
            }
        })
        .collect();
    model.set_vec(rows);
    auto_scroll_subtitle_drawer_if_needed(app, subtitles);
}

fn search_entries(
    entries: &[memento_subtitle::SubtitleEntry],
    query: &str,
    ignore_whitespace: bool,
    remove_regex: Option<&memento_dict::normalize::Regex>,
) -> Vec<usize> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let normalized_query: String = if ignore_whitespace {
        trimmed
            .chars()
            .filter(|c| !c.is_whitespace())
            .flat_map(char::to_lowercase)
            .collect()
    } else {
        trimmed.chars().flat_map(char::to_lowercase).collect()
    };
    if normalized_query.is_empty() {
        return Vec::new();
    }
    entries
        .iter()
        .enumerate()
        .filter_map(|(idx, cue)| {
            let raw = String::from_utf16_lossy(&cue.text);
            let text = memento_dict::normalize::apply_compiled_remove_regex(&raw, remove_regex);
            let haystack: String = if ignore_whitespace {
                text.chars()
                    .filter(|c| !c.is_whitespace())
                    .flat_map(char::to_lowercase)
                    .collect()
            } else {
                text.chars().flat_map(char::to_lowercase).collect()
            };
            haystack.contains(&normalized_query).then_some(idx)
        })
        .collect()
}

fn auto_seek_to_matched_row(app: &MementoApp, row: usize) {
    if !app.get_subtitle_search_auto_seek() {
        return;
    }
    let model = if app.get_subtitle_drawer_tab() == 1 {
        app.get_secondary_subtitle_entries()
    } else {
        app.get_subtitle_entries()
    };
    if let Some(entry) = model.row_data(row) {
        app.invoke_seek_to(entry.start.max(0.0));
    }
}

static CLIPBOARD_HOLDER: std::sync::OnceLock<Mutex<Option<copypasta::ClipboardContext>>> =
    std::sync::OnceLock::new();
static FALLBACK_CLIPBOARD: std::sync::OnceLock<Mutex<String>> = std::sync::OnceLock::new();

pub fn copy_text_to_clipboard(text: &str) {
    if let Ok(mut fallback) = FALLBACK_CLIPBOARD
        .get_or_init(|| Mutex::new(String::new()))
        .lock()
    {
        *fallback = text.to_owned();
    }
    use copypasta::ClipboardProvider;
    let _ = std::panic::catch_unwind(|| {
        let holder = CLIPBOARD_HOLDER.get_or_init(|| Mutex::new(None));
        if let Ok(mut guard) = holder.lock() {
            if guard.is_none() {
                *guard = copypasta::ClipboardContext::new().ok();
            }
            if let Some(ctx) = guard.as_mut() {
                let _ = ctx.set_contents(text.to_owned());
            }
        }
    });
}

pub fn read_copied_text_or_clipboard() -> String {
    let clip = controller::anki::read_clipboard_text();
    if !clip.is_empty() {
        return clip;
    }
    FALLBACK_CLIPBOARD
        .get()
        .and_then(|m| m.lock().ok().map(|s| s.clone()))
        .unwrap_or_default()
}

fn install_subtitle_selection(
    app: &MementoApp,
    subtitles: Rc<RefCell<SubtitleController>>,
    rows: Rc<VecModel<SubEntry>>,
) {
    let search_matches: Rc<RefCell<Vec<usize>>> = Rc::new(RefCell::new(Vec::new()));

    let controller = subtitles.clone();
    let model = rows.clone();
    app.on_select_row(move |row, extend_range| {
        let Ok(index) = usize::try_from(row) else { return; };
        let mut subtitles = controller.borrow_mut();
        subtitles.select_row(index, extend_range);
        refresh_subtitle_marks(&model, &subtitles);
    });
    let controller = subtitles.clone();
    let model = rows.clone();
    let drag_marks = RefCell::new((0, Vec::new()));
    app.on_select_subtitle_range(move |first, last, begin| {
        let (Ok(first), Ok(last)) = (usize::try_from(first), usize::try_from(last)) else { return; };
        let mut subtitles = controller.borrow_mut();
        let mut previous = drag_marks.borrow_mut();
        if begin { *previous = (subtitles.primary_sid(), subtitles.selected_indices().to_vec()); }
        if previous.0 != subtitles.primary_sid() { return; }
        subtitles.select_range(first, last, &previous.1);
        refresh_subtitle_marks(&model, &subtitles);
    });
    let controller = subtitles.clone();
    let model = rows.clone();
    app.on_mark_subtitle_row(move |row| {
        let Ok(index) = usize::try_from(row) else { return; };
        let mut subtitles = controller.borrow_mut();
        subtitles.set_selected(index, true);
        refresh_subtitle_marks(&model, &subtitles);
    });

    let search_subs = subtitles.clone();
    let search_rows = rows.clone();
    let search_matches_rc = search_matches.clone();
    let weak = app.as_weak();
    app.on_search_subtitles(move |query| {
        let Some(app) = weak.upgrade() else { return; };
        app.set_subtitle_search_query(query.clone());
        let active_tab = app.get_subtitle_drawer_tab();
        let ignore_whitespace = app.get_subtitle_search_ignore_whitespace();
        let remove_regex = app.get_search_remove_regex();
        let compiled_regex = memento_dict::normalize::compile_remove_regex(remove_regex.as_str());
        let matching = if query.trim().is_empty() {
            Vec::new()
        } else if active_tab == 1 {
            let subs = search_subs.borrow();
            let sec_matches = search_entries(
                subs.secondary_entries(),
                &query,
                ignore_whitespace,
                compiled_regex.as_ref(),
            );
            if !sec_matches.is_empty() || !subs.secondary_entries().is_empty() {
                sec_matches
            } else {
                let sec_model = app.get_secondary_subtitle_entries();
                let q_norm: String = if ignore_whitespace {
                    query
                        .trim()
                        .chars()
                        .filter(|c| !c.is_whitespace())
                        .flat_map(char::to_lowercase)
                        .collect()
                } else {
                    query.trim().chars().flat_map(char::to_lowercase).collect()
                };
                (0..sec_model.row_count())
                    .filter(|&i| {
                        sec_model.row_data(i).is_some_and(|r| {
                            let h: String = if ignore_whitespace {
                                r.text
                                    .chars()
                                    .filter(|c| !c.is_whitespace())
                                    .flat_map(char::to_lowercase)
                                    .collect()
                            } else {
                                r.text.chars().flat_map(char::to_lowercase).collect()
                            };
                            h.contains(&q_norm)
                        })
                    })
                    .collect()
            }
        } else if compiled_regex.is_some() {
            search_entries(
                search_subs.borrow().primary_entries(),
                &query,
                ignore_whitespace,
                compiled_regex.as_ref(),
            )
        } else {
            search_subs.borrow().search(&query, ignore_whitespace)
        };

        if active_tab == 1 {
            let sec_model = app.get_secondary_subtitle_entries();
            for index in 0..sec_model.row_count() {
                if let Some(mut row) = sec_model.row_data(index) {
                    let is_match = matching.contains(&index);
                    if row.search_match != is_match {
                        row.search_match = is_match;
                        sec_model.set_row_data(index, row);
                    }
                }
            }
            for index in 0..search_rows.row_count() {
                if let Some(mut row) = search_rows.row_data(index) {
                    if row.search_match {
                        row.search_match = false;
                        search_rows.set_row_data(index, row);
                    }
                }
            }
        } else {
            for index in 0..search_rows.row_count() {
                if let Some(mut row) = search_rows.row_data(index) {
                    let is_match = matching.contains(&index);
                    if row.search_match != is_match {
                        row.search_match = is_match;
                        search_rows.set_row_data(index, row);
                    }
                }
            }
        }

        let total = matching.len() as i32;
        app.set_subtitle_search_total_matches(total);
        if let Some(&first_row) = matching.first() {
            app.set_subtitle_search_current_match(1);
            app.set_subtitle_search_focused_row(first_row as i32);
            scroll_drawer_to_row_accurate(&app, first_row);
            auto_seek_to_matched_row(&app, first_row);
        } else {
            app.set_subtitle_search_current_match(0);
            app.set_subtitle_search_focused_row(-1);
        }
        *search_matches_rc.borrow_mut() = matching;
    });

    let next_matches = search_matches.clone();
    let next_weak = app.as_weak();
    app.on_next_subtitle_search_match(move || {
        let Some(app) = next_weak.upgrade() else { return; };
        let matches = next_matches.borrow();
        if matches.is_empty() {
            return;
        }
        let len = matches.len();
        let current = app.get_subtitle_search_current_match().max(1) as usize;
        let next_idx = current % len;
        let row = matches[next_idx];
        app.set_subtitle_search_current_match((next_idx + 1) as i32);
        app.set_subtitle_search_focused_row(row as i32);
        scroll_drawer_to_row_accurate(&app, row);
        auto_seek_to_matched_row(&app, row);
    });

    let prev_matches = search_matches.clone();
    let prev_weak = app.as_weak();
    app.on_prev_subtitle_search_match(move || {
        let Some(app) = prev_weak.upgrade() else { return; };
        let matches = prev_matches.borrow();
        if matches.is_empty() {
            return;
        }
        let len = matches.len();
        let current = app.get_subtitle_search_current_match().max(1) as usize;
        let prev_idx = (current + len - 2) % len;
        let row = matches[prev_idx];
        app.set_subtitle_search_current_match((prev_idx + 1) as i32);
        app.set_subtitle_search_focused_row(row as i32);
        scroll_drawer_to_row_accurate(&app, row);
        auto_seek_to_matched_row(&app, row);
    });

    let tab_weak = app.as_weak();
    let tab_subs = subtitles.clone();
    app.on_subtitle_drawer_tab_changed(move |_tab| {
        let Some(app) = tab_weak.upgrade() else { return; };
        let q = app.get_subtitle_search_query();
        app.invoke_search_subtitles(q.clone());
        if q.trim().is_empty() {
            if let Ok(subs) = tab_subs.try_borrow() {
                auto_scroll_subtitle_drawer_if_needed(&app, &subs);
            }
        }
    });

    let user_scroll_timer = Rc::new(slint::Timer::default());
    let scroll_timer_handle = user_scroll_timer.clone();
    let scroll_weak = app.as_weak();
    app.on_subtitle_drawer_user_scrolled(move || {
        let weak = scroll_weak.clone();
        scroll_timer_handle.start(
            slint::TimerMode::SingleShot,
            std::time::Duration::from_millis(2500),
            move || {
                if let Some(app) = weak.upgrade() {
                    app.set_subtitle_drawer_user_scrolling(false);
                }
            },
        );
    });

    let copy_subs = subtitles.clone();
    let copy_weak = app.as_weak();
    app.on_copy_selected_subtitles(move || {
        let Some(app) = copy_weak.upgrade() else { return; };
        let subs = copy_subs.borrow();
        let remove_regex = app.get_search_remove_regex();
        let selected_cues = subs.selected_cues();
        let text = if !selected_cues.is_empty() {
            selected_cues
                .iter()
                .map(|c| {
                    let raw = String::from_utf16_lossy(&c.text);
                    controller::dict::apply_remove_regex(&raw, remove_regex.as_str())
                })
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            let active = app.get_active_sub_text().to_string();
            if !active.is_empty() {
                active
            } else {
                subs.active_cue_text()
                    .map(|raw| controller::dict::apply_remove_regex(&raw, remove_regex.as_str()))
                    .unwrap_or_default()
            }
        };
        if !text.is_empty() {
            copy_text_to_clipboard(&text);
            app.set_copied_subtitle_text(text.into());
            app.set_status_message("Copied subtitle to clipboard".into());
        }
    });
}

#[cfg(test)]
mod subtitle_layout_tests {
    use super::*;

    #[test]
    fn context_marks_clear_when_playback_reaches_the_next_subtitle() {
        i_slint_backend_testing::init_no_event_loop();
        let app = MementoApp::new().unwrap();
        let rows = VecModel::<SubEntry>::default();
        let mut subtitles = SubtitleController::new();
        let mut previous = None;
        subtitles.load_cues(vec![
            controller::subtitles::create_cue("Repeated line", 1., 2.),
            controller::subtitles::create_cue("Repeated line", 3., 4.),
            controller::subtitles::create_cue("Last", 5., 6.),
        ]);
        subtitles.update_position(1.5);
        clear_subtitle_marks_on_advance(&rows, &mut subtitles, &mut previous, true);
        subtitles.select_range(0, 2, &[]);
        refresh_subtitle_list(&app, &rows, &subtitles);
        for position in [1.8, 2.5] {
            subtitles.update_position(position);
            clear_subtitle_marks_on_advance(&rows, &mut subtitles, &mut previous, false);
            assert_eq!(subtitles.selected_indices(), &[0, 1, 2], "resuming and the gap retain marks until the next cue");
        }
        subtitles.update_position(3.5);
        clear_subtitle_marks_on_advance(&rows, &mut subtitles, &mut previous, false);
        assert!(subtitles.selected_indices().is_empty());
        assert!(rows.iter().all(|row| !row.selected));
        subtitles.select_all();
        subtitles.update_position(5.5);
        clear_subtitle_marks_on_advance(&rows, &mut subtitles, &mut previous, true);
        clear_subtitle_marks_on_advance(&rows, &mut subtitles, &mut previous, false);
        assert_eq!(subtitles.selected_indices(), &[0, 1, 2], "a paused seek and resume on the same cue retain marks");

        subtitles.start_asr_track();
        let cue = |id, start| MemorySubtitleCue {
            id, text: "ASR".encode_utf16().collect(), start, end: start + 1.,
        };
        subtitles.add_asr_cue(cue(1, 3.), 3.5);
        clear_subtitle_marks_on_advance(&rows, &mut subtitles, &mut previous, true);
        subtitles.select_all();
        // Background ASR arrivals may shift row indices, but don't mean
        // playback has moved to another subtitle.
        subtitles.add_asr_cue(cue(2, 1.), 3.5);
        subtitles.add_asr_cue(cue(3, 5.), 3.5);
        refresh_subtitle_list(&app, &rows, &subtitles);
        clear_subtitle_marks_on_advance(&rows, &mut subtitles, &mut previous, false);
        assert_eq!(subtitles.selected_indices(), &[1]);
        subtitles.update_position(5.5);
        clear_subtitle_marks_on_advance(&rows, &mut subtitles, &mut previous, false);
        assert!(subtitles.selected_indices().is_empty());
        assert!(rows.iter().all(|row| !row.selected));
    }

    #[test]
    fn drawer_marks_multiple_cues_without_search_or_playback_overwriting_them() {
        use slint::platform::{PointerEventButton, WindowEvent};
        i_slint_backend_testing::init_no_event_loop();
        let app = MementoApp::new().unwrap();
        app.window().set_size(slint::LogicalSize::new(1280., 720.));
        app.set_duration(120.);
        app.set_sub_drawer_open(true);
        let subtitles = Rc::new(RefCell::new(SubtitleController::new()));
        subtitles.borrow_mut().load_cues(vec![
            controller::subtitles::create_cue("First", 1., 2.),
            controller::subtitles::create_cue("Middle", 3., 4.),
            controller::subtitles::create_cue("Last", 5., 6.),
        ]);
        let rows = Rc::new(VecModel::<SubEntry>::default());
        app.set_subtitle_entries(rows.clone().into());
        refresh_subtitle_list(&app, &rows, &subtitles.borrow());
        install_subtitle_selection(&app, subtitles.clone(), rows.clone());
        app.show().unwrap();
        let click = |x, y| {
            let position = slint::LogicalPosition::new(x, y);
            app.window().dispatch_event(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
            app.window().dispatch_event(WindowEvent::PointerReleased { position, button: PointerEventButton::Left });
        };
        click(950., 94.);
        assert_eq!(subtitles.borrow().selected_indices(), &[0]);
        app.window().dispatch_event(WindowEvent::KeyPressed { text: slint::platform::Key::Shift.into() });
        click(950., 166.);
        app.window().dispatch_event(WindowEvent::KeyReleased { text: slint::platform::Key::Shift.into() });
        assert_eq!(subtitles.borrow().selected_indices(), &[0, 1, 2]);
        click(1100., 130.);
        assert_eq!(subtitles.borrow().selected_indices(), &[0, 2]);
        app.invoke_search_subtitles("Middle".into());
        assert!(rows.row_data(1).unwrap().search_match);
        assert!(!rows.row_data(1).unwrap().selected, "search matches are not Anki marks");
        subtitles.borrow_mut().update_position(3.5);
        refresh_subtitle_list(&app, &rows, &subtitles.borrow());
        assert!(rows.row_data(1).unwrap().is_active);
        assert!(!rows.row_data(1).unwrap().selected);
        assert!(rows.row_data(0).unwrap().selected && rows.row_data(2).unwrap().selected);
        app.invoke_search_subtitles("".into());
        assert!(rows.iter().all(|row| !row.search_match));
        // A double-click seeks and keeps the destination marked.
        let seek = Rc::new(Cell::new(0.));
        let result = seek.clone();
        app.on_seek_to(move |position| result.set(position));
        click(1100., 166.);
        click(1100., 166.);
        assert_eq!(seek.get(), 5.);
        assert!(subtitles.borrow().is_selected(2));
        subtitles.borrow_mut().clear_selection();
        refresh_subtitle_marks(&rows, &subtitles.borrow());
        assert!(rows.iter().all(|row| !row.selected));
        // Hold and drag across rows; updates happen before releasing. Reversing
        // the drag shrinks its range and preserves marks from earlier clicks.
        app.invoke_mark_subtitle_row(2);
        let position = slint::LogicalPosition::new(1100., 94.);
        app.window().dispatch_event(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
        app.window().dispatch_event(WindowEvent::PointerMoved { position: slint::LogicalPosition::new(1100., 166.) });
        assert_eq!(subtitles.borrow().selected_indices(), &[0, 1, 2]);
        app.window().dispatch_event(WindowEvent::PointerMoved { position });
        assert_eq!(subtitles.borrow().selected_indices(), &[0, 2]);
        app.window().dispatch_event(WindowEvent::PointerReleased { position, button: PointerEventButton::Left });
        assert_eq!(subtitles.borrow().selected_indices(), &[0, 2], "release must not toggle the start row");
        subtitles.borrow_mut().clear_selection();
        refresh_subtitle_marks(&rows, &subtitles.borrow());
        let position = slint::LogicalPosition::new(1100., 166.);
        app.window().dispatch_event(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
        let position = slint::LogicalPosition::new(1100., 94.);
        app.window().dispatch_event(WindowEvent::PointerMoved { position });
        assert_eq!(subtitles.borrow().selected_indices(), &[0, 1, 2], "upward drag must also mark a range");
        app.window().dispatch_event(WindowEvent::PointerReleased { position, button: PointerEventButton::Left });
        assert_eq!(subtitles.borrow().selected_indices(), &[0, 1, 2]);
        // Hovering after release does not change marks, and an ordinary click
        // can still unmark one row after a completed drag.
        app.window().dispatch_event(WindowEvent::PointerMoved { position: slint::LogicalPosition::new(1100., 130.) });
        assert_eq!(subtitles.borrow().selected_indices(), &[0, 1, 2]);
        click(1100., 130.);
        assert_eq!(subtitles.borrow().selected_indices(), &[0, 2]);
        // A long press after scrolling must use the actual cue indices,
        // rather than treating the first visible row as cue zero.
        subtitles.borrow_mut().load_cues((0..100).map(|index| {
            controller::subtitles::create_cue(&format!("Cue {index}"), index as f64 * 2., index as f64 * 2. + 1.)
        }).collect::<Vec<_>>());
        refresh_subtitle_list(&app, &rows, &subtitles.borrow());
        let position = slint::LogicalPosition::new(1100., 94.);
        app.window().dispatch_event(WindowEvent::PointerScrolled { position, delta_x: 0., delta_y: -360. });
        i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(500));
        app.window().dispatch_event(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
        i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(600));
        let position = slint::LogicalPosition::new(1100., 166.);
        app.window().dispatch_event(WindowEvent::PointerMoved { position });
        app.window().dispatch_event(WindowEvent::PointerReleased { position, button: PointerEventButton::Left });
        let selection = subtitles.borrow().selected_indices().to_vec();
        assert_eq!(selection.len(), 3);
        assert!(selection[0] > 0, "wheel scrolling must still work before dragging");
        assert_eq!(selection, (selection[0]..=selection[0] + 2).collect::<Vec<_>>());
        app.hide().unwrap();
    }

    #[test]
    fn mpv_track_sync_preserves_asr_and_settings_reset_only_clears_its_cache() {
        i_slint_backend_testing::init_no_event_loop();
        let app = MementoApp::new().unwrap();
        app.set_asr_enabled(true);
        let asr = Rc::new(RefCell::new(controller::asr::AsrController::new()));
        let subtitles = Rc::new(RefCell::new(SubtitleController::new()));
        let rows = Rc::new(VecModel::<SubEntry>::default());
        let restart = Rc::new(Cell::new(false));
        {
            let mut subtitles = subtitles.borrow_mut();
            subtitles.ensure_track(2);
            subtitles.set_track_cues(2, vec![controller::subtitles::create_cue("File", 1.0, 3.0)]);
            subtitles.start_asr_track();
            subtitles.add_asr_cue(MemorySubtitleCue {
                id: 1, text: "ASR".encode_utf16().collect(), start: 1.0, end: 3.0,
            }, 2.0);
        }
        app.set_asr_track_selected(true);
        app.set_asr_running(true);
        let mut state = memento_player::mpv::PlaybackState::default();
        state.position = 2.0;
        let mut audio = Vec::new();
        let mut tracks = Vec::new();
        let _ = sync_player_tracks_with_asr(&app, &state, &mut subtitles.borrow_mut(), &rows, &mut audio, &mut tracks, None);
        assert_eq!(subtitles.borrow().primary_sid(), ASR_TRACK_ID);
        assert_eq!(subtitles.borrow().active_cue_text().as_deref(), Some("ASR"));
        // Worker completion/pause (`asr_running = false`) keeps ASR cues visible while `asr_track_selected` stays true.
        app.set_asr_running(false);
        let _ = sync_player_tracks_with_asr(&app, &state, &mut subtitles.borrow_mut(), &rows, &mut audio, &mut tracks, None);
        assert_eq!(subtitles.borrow().primary_sid(), ASR_TRACK_ID);
        assert_eq!(subtitles.borrow().active_cue_text().as_deref(), Some("ASR"));
        // Deselecting ASR switches cleanly to track 2.
        app.set_asr_track_selected(false);
        state.primary_sid = 2;
        let _ = sync_player_tracks_with_asr(&app, &state, &mut subtitles.borrow_mut(), &rows, &mut audio, &mut tracks, None);
        assert_eq!(rows.row_data(0).unwrap().text, "File");
        app.set_active_sub_text("File".into());
        clear_asr_for_settings(&app, &asr, &subtitles, &rows, &restart, None, None);
        assert_eq!(app.get_active_sub_text(), "File");
        assert_eq!(rows.row_data(0).unwrap().text, "File");
        assert!(!restart.get());
        assert!(!subtitles.borrow_mut().resume_asr_track(2.0));
        // Reset while ASR is selected queues a fresh model and clears its view.
        subtitles.borrow_mut().start_asr_track();
        app.set_asr_track_selected(true);
        app.set_asr_running(true);
        clear_asr_for_settings(&app, &asr, &subtitles, &rows, &restart, None, None);
        assert!(restart.get());
        assert!(app.get_asr_track_selected());
        assert!(!app.get_asr_running());
        assert!(rows.row_count() == 0);
        assert!(app.get_active_sub_text().is_empty());
    }

    #[test]
    fn bomless_utf16_and_utf8_cjk_subtitle_files_decode_accurately() {
        // Leading CJK character `一` (U+4E00 -> LE `[0x00, 0x4E]`, BE `[0x4E, 0x00]`)
        // has a zero byte in the first code unit and non-ASCII first character!
        let sample = "一話「旅立ち」\n1\n00:00:01,000 --> 00:00:03,000\n世界を旅する\n";
        let utf16le: Vec<u8> = sample.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let utf16be: Vec<u8> = sample.encode_utf16().flat_map(u16::to_be_bytes).collect();
        assert_eq!(decode_subtitle_bytes(&utf16le), sample);
        assert_eq!(decode_subtitle_bytes(&utf16be), sample);
        assert_eq!(decode_subtitle_bytes(sample.as_bytes()), sample);

        let dir = tempfile::TempDir::new().unwrap();
        let srt_le = dir.path().join("cjk_le.srt");
        let srt_be = dir.path().join("cjk_be.srt");
        let srt_content = "1\n00:00:01,000 --> 00:00:03,000\n一話「旅立ち」\n";
        std::fs::write(
            &srt_le,
            srt_content.encode_utf16().flat_map(u16::to_le_bytes).collect::<Vec<u8>>(),
        )
        .unwrap();
        std::fs::write(
            &srt_be,
            srt_content.encode_utf16().flat_map(u16::to_be_bytes).collect::<Vec<u8>>(),
        )
        .unwrap();
        let cues_le = parse_external_subtitles(&srt_le).unwrap();
        let cues_be = parse_external_subtitles(&srt_be).unwrap();
        assert_eq!(cues_le.len(), 1);
        assert_eq!(String::from_utf16_lossy(&cues_le[0].text), "一話「旅立ち」");
        assert_eq!(cues_be.len(), 1);
        assert_eq!(String::from_utf16_lossy(&cues_be[0].text), "一話「旅立ち」");
    }

    #[test]
    fn furigana_subtitle_wraps_within_narrow_video_without_losing_text() {
        let parts = [
            ("生前", "せいぜん"), ("に", ""), ("世界", "せかい"), ("中", "じゅう"),
            ("を", ""), ("旅行", "りょこう"), ("し", ""), ("て", ""),
            ("集め", "あつめ"), ("た", ""), ("遺品", "いひん"), ("が", ""),
        ];
        let words: Vec<_> = parts.iter().enumerate().map(|(index, (text, reading))| SubtitleWord {
            text: (*text).into(), reading: (*reading).into(), query: (*text).into(), index: index as i32,
        }).collect();
        let lines = subtitle_lines_from_words(words, 428.0, 38.0, true);
        assert!(lines.len() >= 2);
        let mut rebuilt = String::new();
        for line in lines {
            let line_words: Vec<_> = line.words.iter().collect();
            let width: f32 = line_words.iter().map(|word| subtitle_token_width(word, 38.0, true)).sum();
            assert!(width <= 428.0, "subtitle line exceeds video width: {width}");
            for word in line_words { rebuilt.push_str(word.text.as_str()); }
        }
        assert_eq!(rebuilt, "生前に世界中を旅行して集めた遺品が");
    }
}

fn is_bare_modifier_key(key: &str) -> bool {
    matches!(
        key,
        ""
            | "\u{0010}"
            | "\u{0011}"
            | "\u{0012}"
            | "\u{0013}"
            | "\u{0014}"
            | "\u{0015}"
            | "\u{0016}"
            | "\u{0017}"
            | "\u{0018}"
            | "Shift"
            | "Control"
            | "Ctrl"
            | "Alt"
            | "Meta"
            | "Super"
    )
}

fn bare_modifier_flags(key: &str) -> (bool, bool, bool, bool) {
    use slint::platform::Key;
    if let Some(ch) = key.chars().next().filter(|_| key.chars().count() == 1) {
        if ch == char::from(Key::Shift) || ch == char::from(Key::ShiftR) {
            return (false, true, false, false);
        } else if ch == char::from(Key::Control) || ch == char::from(Key::ControlR) {
            return (true, false, false, false);
        } else if ch == char::from(Key::Alt) || ch == char::from(Key::AltGr) {
            return (false, false, true, false);
        } else if ch == char::from(Key::Meta) || ch == char::from(Key::MetaR) {
            return (false, false, false, true);
        } else if ch == char::from(Key::CapsLock) {
            return (false, false, false, false);
        }
    }
    match key {
        "Shift" | "ShiftR" => (false, true, false, false),
        "Control" | "Ctrl" => (true, false, false, false),
        "Alt" => (false, false, true, false),
        "Meta" | "Super" => (false, false, false, true),
        _ => (false, false, false, false),
    }
}

fn translate_slint_key_name(key: &str) -> Option<&'static str> {
    match key {
        "\u{F700}" => Some("Up"),
        "\u{F701}" => Some("Down"),
        "\u{F702}" => Some("Left"),
        "\u{F703}" => Some("Right"),
        "\u{F704}" => Some("F1"),
        "\u{F705}" => Some("F2"),
        "\u{F706}" => Some("F3"),
        "\u{F707}" => Some("F4"),
        "\u{F708}" => Some("F5"),
        "\u{F709}" => Some("F6"),
        "\u{F70A}" => Some("F7"),
        "\u{F70B}" => Some("F8"),
        "\u{F70C}" => Some("F9"),
        "\u{F70D}" => Some("F10"),
        "\u{F70E}" => Some("F11"),
        "\u{F70F}" => Some("F12"),
        "\u{007f}" | "\u{F728}" => Some("Delete"),
        "\u{001b}" => Some("Escape"),
        "\u{0008}" => Some("Backspace"),
        "\u{0009}" | "\u{0019}" => Some("Tab"),
        "\u{000a}" | "\r" => Some("Return"),
        " " => Some("Space"),
        _ => None,
    }
}

fn keyboard_chord(
    key: &str,
    control: bool,
    shift: bool,
    alt: bool,
    meta: bool,
) -> String {
    if is_bare_modifier_key(key) {
        return String::new();
    }
    let mut chord = String::new();
    if control { chord.push_str("Ctrl+"); }
    if alt { chord.push_str("Alt+"); }
    if shift { chord.push_str("Shift+"); }
    if meta { chord.push_str("Meta+"); }
    if let Some(named) = translate_slint_key_name(key) {
        chord.push_str(named);
    } else {
        chord.push_str(&key.to_uppercase());
    }
    chord
}

fn chapter_marks_for_duration(chapters: &[f64], duration: f64) -> Vec<ChapterMark> {
    if duration <= 0.0 {
        return Vec::new();
    }
    chapters
        .iter()
        .enumerate()
        .filter(|(_, &time)| time >= 0.0 && time <= duration)
        .map(|(index, &time)| ChapterMark {
            time: time as f32,
            title: format!("Chapter {}", index + 1).into(),
        })
        .collect()
}

fn chapter_title_at_time(app: &MementoApp, time_sec: f32) -> String {
    let marks = app.get_chapter_marks();
    let mut best_title = String::new();
    let mut best_time = -1.0f32;
    for i in 0..marks.row_count() {
        if let Some(mark) = marks.row_data(i) {
            if mark.time <= time_sec + 0.05 && mark.time >= best_time {
                best_time = mark.time;
                best_title = if mark.title.is_empty() {
                    format!("Chapter {}", i + 1)
                } else {
                    mark.title.to_string()
                };
            }
        }
    }
    if !best_title.is_empty() {
        return best_title;
    }
    let chapters = app.get_chapters();
    for i in (0..chapters.row_count()).rev() {
        if let Some(ch_time) = chapters.row_data(i) {
            if ch_time <= time_sec + 0.05 {
                return format!("Chapter {}", i + 1);
            }
        }
    }
    String::new()
}

fn setting_number(config: &ConfigController, group: &str, key: &str, default: f64) -> f32 {
    match config.effective_setting(group, key) {
        Some(Value::Real(value)) => value as f32,
        Some(Value::Int(value)) => value as f32,
        _ => default as f32,
    }
}

fn setting_enabled(config: &ConfigController, group: &str, key: &str, default: bool) -> bool {
    match config.effective_setting(group, key) {
        Some(Value::Bool(value)) => value,
        Some(Value::Int(value)) => value != 0,
        _ => default,
    }
}

fn setting_text(config: &ConfigController, group: &str, key: &str, default: &str) -> String {
    match config.effective_setting(group, key) {
        Some(Value::Text(value)) => value,
        _ => default.to_owned(),
    }
}

fn setting_color(
    config: &ConfigController,
    group: &str,
    key: &str,
    default_hex: &str,
) -> slint::Color {
    let raw_hex = setting_text(config, group, key, default_hex);
    let parsed = ArgbColor::parse(&raw_hex)
        .or_else(|_| ArgbColor::parse(default_hex))
        .unwrap_or_else(|_| ArgbColor::new(255, 0, 0, 0));
    slint::Color::from_argb_u8(parsed.a, parsed.r, parsed.g, parsed.b)
}

fn setting_font_spec(
    config: &ConfigController,
    group: &str,
    key: &str,
    default_family: &str,
    default_size: f32,
) -> (String, f32, bool, bool) {
    let raw_font = setting_text(config, group, key, "");
    match QtFontSpec::parse(&raw_font) {
        Ok(spec) => (
            spec.family,
            (spec.point_size as f32).clamp(8.0, 72.0),
            spec.bold,
            spec.italic,
        ),
        Err(_) => (default_family.to_owned(), default_size, false, false),
    }
}

fn refresh_recent_files(app: &MementoApp, config: &ConfigController) {
    let raw_files = config.recent_files();
    let titles = recent_titles_map()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let files: Vec<slint::SharedString> = raw_files.iter().cloned().map(Into::into).collect();
    let labels: Vec<slint::SharedString> = raw_files
        .iter()
        .map(|path| format_recent_file_label(titles.get(path).map_or("", String::as_str), path).into())
        .collect();
    drop(titles);
    app.set_recent_files(Rc::new(VecModel::from(files)).into());
    app.set_recent_file_labels(Rc::new(VecModel::from(labels)).into());
}

fn refresh_player_preferences(app: &MementoApp, config: &ConfigController) {
    app.set_hide_mpv_subtitles(setting_enabled(
        config, raw::search::GROUP, raw::search::HIDE_MPV_SUBS,
        raw::search::HIDE_MPV_SUBS_DEFAULT,
    ));
    app.set_hide_memento_while_playing(setting_enabled(
        config, raw::search::GROUP, raw::search::HIDE_SUBS,
        raw::search::HIDE_SUBS_DEFAULT,
    ));
    app.set_hover_pause_enabled(setting_enabled(
        config, raw::search::GROUP, raw::search::PAUSE_ON_HOVER,
        raw::search::PAUSE_ON_HOVER_DEFAULT,
    ));
    app.set_auto_play_audio_enabled(setting_enabled(
        config, raw::search::GROUP, raw::search::AUTO_PLAY_AUDIO,
        raw::search::AUTO_PLAY_AUDIO_DEFAULT,
    ));
    let (remove_regex, replace_lines, replace_with) = search_text_filters(config);
    app.set_search_remove_regex(remove_regex.into());
    app.set_search_replace_lines(replace_lines);
    app.set_search_replace_with(replace_with.into());
    app.set_furigana_visible(setting_enabled(
        config, raw::search::GROUP, raw::search::SUBTITLE_FURIGANA,
        raw::search::SUBTITLE_FURIGANA_DEFAULT,
    ));
    app.set_furigana_enabled(true);
    app.set_autofit_enabled(setting_enabled(
        config, raw::behavior::GROUP, raw::behavior::AUTO_FIT,
        raw::behavior::AUTO_FIT_DEFAULT,
    ));
    app.set_autofit_percent(
        setting_number(
            config,
            raw::behavior::GROUP,
            raw::behavior::AUTO_FIT_PERCENT,
            raw::behavior::AUTO_FIT_PERCENT_DEFAULT as f64,
        )
        .round()
        .clamp(1.0, 1000.0) as i32,
    );
    app.set_osc_duration_ms(setting_number(
        config, raw::behavior::GROUP, raw::behavior::OSC_DURATION,
        raw::behavior::OSC_DURATION_DEFAULT as f64,
    ).round().clamp(50.0, 9999.0) as i32);
    app.set_osc_fade_ms(setting_number(
        config, raw::behavior::GROUP, raw::behavior::OSC_FADE,
        raw::behavior::OSC_FADE_DEFAULT as f64,
    ).round().clamp(0.0, 9999.0) as i32);
    app.set_osc_min_move(setting_number(
        config, raw::behavior::GROUP, raw::behavior::OSC_MIN_MOVE,
        raw::behavior::OSC_MIN_MOVE_DEFAULT as f64,
    ).round().max(0.0) as i32);
    app.set_osc_mpv_cursor(setting_enabled(
        config, raw::behavior::GROUP, raw::behavior::OSC_MPV_CURSOR,
        raw::behavior::OSC_MPV_CURSOR_DEFAULT,
    ));
    app.set_subtitle_cursor_show_enabled(setting_enabled(
        config, raw::behavior::GROUP, raw::behavior::SUBTITLE_CURSOR_SHOW,
        raw::behavior::SUBTITLE_CURSOR_SHOW_DEFAULT,
    ));
    app.set_secondary_subtitle_cursor_show_enabled(setting_enabled(
        config,
        raw::behavior::GROUP,
        raw::behavior::SECONDARY_SUBTITLE_CURSOR_SHOW,
        raw::behavior::SECONDARY_SUBTITLE_CURSOR_SHOW_DEFAULT,
    ));
    app.set_preview_thumbnails_enabled(setting_enabled(
        config,
        raw::behavior::GROUP,
        raw::behavior::OSC_PREVIEW_THUMBNAILS,
        raw::behavior::OSC_PREVIEW_THUMBNAILS_DEFAULT,
    ));
    app.set_sub_list_show_timestamps(setting_enabled(
        config,
        raw::interface::GROUP,
        raw::interface::subtitle_list::TIMESTAMPS,
        raw::interface::subtitle_list::TIMESTAMPS_DEFAULT,
    ));
    app.set_subtitle_search_ignore_whitespace(setting_enabled(
        config,
        raw::subtitle_list::GROUP,
        raw::subtitle_list::IGNORE_WHITESPACE,
        raw::subtitle_list::IGNORE_WHITESPACE_DEFAULT,
    ));
    app.set_subtitle_search_auto_seek(setting_enabled(
        config,
        raw::subtitle_list::GROUP,
        raw::subtitle_list::AUTO_SEEK,
        raw::subtitle_list::AUTO_SEEK_DEFAULT,
    ));
    app.set_sub_list_bg_color(setting_color(
        config,
        raw::interface::GROUP,
        raw::interface::subtitle_list::BACKGROUND_COLOR,
        raw::interface::subtitle_list::BACKGROUND_COLOR_DEFAULT,
    ));
    app.set_sub_list_text_color(setting_color(
        config,
        raw::interface::GROUP,
        raw::interface::subtitle_list::TEXT_COLOR,
        raw::interface::subtitle_list::TEXT_COLOR_DEFAULT,
    ));
    app.set_sub_list_hover_color(setting_color(
        config,
        raw::interface::GROUP,
        raw::interface::subtitle_list::HOVER_COLOR,
        raw::interface::subtitle_list::HOVER_COLOR_DEFAULT,
    ));
    app.set_sub_list_selected_color(setting_color(
        config,
        raw::interface::GROUP,
        raw::interface::subtitle_list::SELECTED_COLOR,
        raw::interface::subtitle_list::SELECTED_COLOR_DEFAULT,
    ));
    app.set_sub_list_selected_hover_color(setting_color(
        config,
        raw::interface::GROUP,
        raw::interface::subtitle_list::SELECTED_HOVER_COLOR,
        raw::interface::subtitle_list::SELECTED_HOVER_COLOR_DEFAULT,
    ));
    let (p_fam, p_size, p_bold, p_italic) = setting_font_spec(
        config,
        raw::interface::GROUP,
        raw::interface::subtitle_list::PRIMARY_FONT,
        "Noto Sans CJK JP",
        16.0,
    );
    app.set_sub_list_primary_font_family(p_fam.into());
    app.set_sub_list_primary_font_size(p_size);
    app.set_sub_list_primary_font_bold(p_bold);
    app.set_sub_list_primary_font_italic(p_italic);
    let (s_fam, s_size, s_bold, s_italic) = setting_font_spec(
        config,
        raw::interface::GROUP,
        raw::interface::subtitle_list::SECONDARY_FONT,
        "Sans Serif",
        16.0,
    );
    app.set_sub_list_secondary_font_family(s_fam.into());
    app.set_sub_list_secondary_font_size(s_size);
    app.set_sub_list_secondary_font_bold(s_bold);
    app.set_sub_list_secondary_font_italic(s_italic);
    app.set_popup_configured_width((setting_number(
        config,
        raw::interface::GROUP,
        raw::interface::POPUP_WIDTH,
        raw::interface::POPUP_WIDTH_DEFAULT as f64,
    ).round().clamp(240.0, 960.0)) as f32);
    app.set_popup_configured_height((setting_number(
        config,
        raw::interface::GROUP,
        raw::interface::POPUP_HEIGHT,
        raw::interface::POPUP_HEIGHT_DEFAULT as f64,
    ).round().clamp(180.0, 720.0)) as f32);
    app.set_subtitle_scale(setting_number(
        config, raw::interface::GROUP, raw::interface::subtitle::SCALE,
        raw::interface::subtitle::SCALE_DEFAULT,
    ));
    app.set_subtitle_offset(setting_number(
        config, raw::interface::GROUP, raw::interface::subtitle::OFFSET,
        raw::interface::subtitle::OFFSET_DEFAULT,
    ));
    app.set_auto_pause(setting_enabled(
        config, raw::behavior::GROUP, raw::behavior::SUBTITLE_PAUSE,
        raw::behavior::SUBTITLE_PAUSE_DEFAULT,
    ));
    app.set_anki_enabled(config.anki_enabled());
    app.set_show_anki_checkbox(setting_enabled(
        config, raw::search::GROUP, raw::search::SHOW_ANKI_GLOSSARY_CHECKBOX,
        raw::search::SHOW_ANKI_GLOSSARY_CHECKBOX_DEFAULT,
    ));
    app.set_anki_profile(config.current_anki_profile().into());
    let profiles: Vec<slint::SharedString> = config.anki_profile_names().into_iter().map(Into::into).collect();
    app.set_anki_profiles(Rc::new(VecModel::from(profiles)).into());
    let profile = format!(
        "{}/{}/{}",
        raw::keybind::GROUP, raw::keybind::profile::GROUP,
        config.current_keybind_profile(),
    );
    let shortcut = |key: &str| match config.effective_setting(&profile, key) {
        Some(Value::Text(text)) => text.into(),
        _ => slint::SharedString::default(),
    };
    app.set_key_open_file(shortcut(raw::keybind::profile::OPEN_FILE));
    app.set_key_open_url(shortcut(raw::keybind::profile::OPEN_URL));
    app.set_key_show_search(shortcut(raw::keybind::profile::SHOW_SEARCH));
    app.set_key_show_sub_list(shortcut(raw::keybind::profile::SHOW_SUBTITLE_LIST));
    app.set_key_start_ocr(shortcut(raw::keybind::profile::START_OCR));
    app.set_key_sub_auto_pause(shortcut(raw::keybind::profile::SUBTITLE_AUTO_PAUSE));
    app.set_key_sub_show(shortcut(raw::keybind::profile::SUBTITLE_SHOW));
    app.set_key_sub_furigana(shortcut(raw::keybind::profile::SUBTITLE_FURIGANA));
    app.set_key_sub_increase_size(shortcut(raw::keybind::profile::SUBTITLE_INCREASE_SIZE));
    app.set_key_sub_decrease_size(shortcut(raw::keybind::profile::SUBTITLE_DECREASE_SIZE));
    app.set_key_sub_move_up(shortcut(raw::keybind::profile::SUBTITLE_MOVE_UP));
    app.set_key_sub_move_down(shortcut(raw::keybind::profile::SUBTITLE_MOVE_DOWN));
    app.set_key_card_add(shortcut(raw::keybind::profile::CARD_ADD));
    app.set_key_card_next(shortcut(raw::keybind::profile::CARD_NEXT));
    app.set_key_card_prev(shortcut(raw::keybind::profile::CARD_PREVIOUS));
    app.set_key_osc_visibility(shortcut(raw::keybind::profile::OSC_VISIBILITY));
    app.set_key_sub_list_copy(shortcut(raw::keybind::profile::SUBTITLE_LIST_COPY_SELECTED));
    app.set_key_sub_list_find(shortcut(raw::keybind::profile::SUBTITLE_LIST_FIND));
    app.set_key_sub_list_find_next(shortcut(raw::keybind::profile::SUBTITLE_LIST_FIND_NEXT));
    app.set_key_sub_list_find_prev(shortcut(raw::keybind::profile::SUBTITLE_LIST_FIND_PREVIOUS));
    ui::refresh_rich_ui_bridges(app, config);
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // libmpv requires the C numeric locale (`.` decimal separator) before initialization.
    // SAFETY: invoked on the main thread before any worker threads or GUI/libmpv init.
    unsafe {
        libc::setlocale(libc::LC_NUMERIC, c"C".as_ptr());
    }
    let raw_args: Vec<String> = std::env::args().collect();
    let parsed_args = memento_player::parse_args(&raw_args)
        .map_err(|err| format!("invalid command-line arguments: {err:?}"))?;
    if parsed_args.show_help {
        print!("{}", memento_player::help_text());
        return Ok(());
    }
    if parsed_args.show_version {
        println!("{}", memento_player::version_text(env!("CARGO_PKG_VERSION")));
        return Ok(());
    }
    #[cfg(feature = "ocr")]
    controller::ocr::prepare_python_executable();
    let _ = log::set_logger(&APP_LOGGER);
    log::set_max_level(log::LevelFilter::Warn);
    let config_dir = memento_xdg_dir("XDG_CONFIG_HOME", ".config")
        .ok_or("HOME or XDG_CONFIG_HOME must be set to load Anki profiles")?;
    // Pre-select matching GPU offload env vars from CLI `--hwdec=...` / `mpv.conf`
    // BEFORE creating Slint's OpenGL context or initializing libmpv.
    graphics_device::select_for_config_dir(&config_dir);
    // libmpv shares the UI's context. On desktop platforms prefer desktop GL
    // with the driver's supported version, rather than Slint's GLES 2 default.
    // An explicitly selected Slint backend (including software) still wins.
    if std::env::var_os("SLINT_BACKEND").is_none() {
        slint::BackendSelector::new().require_opengl().select()?;
    }
    let _ = slint::set_xdg_app_id(memento_os::WAYLAND_APP_ID);
    let app = MementoApp::new()?;
    ui::prepare_text_fonts();

    if let Some(cache_dir) = memento_xdg_dir("XDG_CACHE_HOME", ".cache") {
        if let Err(error) = migrate_xdg_config_if_needed(&cache_dir, &config_dir, "memento.conf") {
            eprintln!("Could not migrate player settings from {}: {error}", cache_dir.display());
        }
    }
    let data_dir = memento_xdg_dir("XDG_DATA_HOME", ".local/share")
        .ok_or("HOME or XDG_DATA_HOME must be set to load dictionaries")?;
    let anki_json_path = config_dir.join("anki_connect.json");
    let config = ConfigController::load(&config_dir, "memento.conf")
        .map_err(|error| format!("could not load player settings: {error}"))?;
    config.load_anki_profiles(&anki_json_path)?;
    let config_ctrl = Rc::new(config);

    let dictionary_database_path = data_dir.join("dictionaries.sqlite");
    let dictionary_resources_path = data_dir.join("res");
    let dictionary_manager = match DictionaryManager::new(
        &dictionary_database_path,
        &dictionary_resources_path,
    ) {
        Ok(manager) => Some(Rc::new(manager)),
        Err(error) => {
            eprintln!("Dictionary manager unavailable: {error}");
            None
        }
    };
    let sub_ctrl = Rc::new(RefCell::new(SubtitleController::new()));
    let mut dictionary =
        DictController::open(&dictionary_database_path).unwrap_or_else(|error| {
            eprintln!("Dictionary lookup unavailable: {error}");
            DictController::new()
        });
    apply_dictionary_order(&config_ctrl, &mut dictionary);
    let dict_ctrl = Rc::new(RefCell::new(dictionary));

    // mpv renders into a Slint image, never into the toplevel native window:
    // a native child would occlude the OSC, subtitles and dictionary popup.
    let (mpv, coordinator) = match MpvInstance::new_with_options(
        Some(&config_dir),
        &parsed_args.global_options,
    ) {
        Ok(instance) => {
            let coord = LivePlayerCoordinator::new(&instance);
            (
                Rc::new(RefCell::new(Some(instance))),
                Rc::new(RefCell::new(Some(coord))),
            )
        }
        Err(err) => {
            eprintln!("Could not initialize video playback: {err}");
            (Rc::new(RefCell::new(None)), Rc::new(RefCell::new(None)))
        }
    };
    let screensaver = Rc::new(RefCell::new(memento_os::Screensaver::new()));
    let video_timings = Rc::new(RefCell::new(VideoTimings::default()));
    let pending_playback_frame = Rc::new(Cell::new(None::<i32>));
    let current_render_size = Rc::new(Cell::new((0u32, 0u32)));
    let start_player = mpv.clone();
    let start_app = app.as_weak();
    app.on_video_load_presented(move |generation| {
        let Some(app) = start_app.upgrade() else { return };
        if app.get_pending_playback_generation() != generation { return; }
        // The first base-text frame can be presented before the reading worker
        // replies. Keep the clock paused until that reading has also been laid
        // out and drawn, including the first small-font glyph upload.
        if app.get_subtitle_furigana_pending() { return; }
        app.set_pending_playback_generation(-1);
        if let Some(player) = start_player.borrow().as_ref() {
            if let Err(error) = player.play() { app.set_status_message(error.into()); }
        }
    });

    // Subtitle models (primary & secondary)
    let sub_model = Rc::new(slint::VecModel::<SubEntry>::default());
    app.set_subtitle_entries(sub_model.clone().into());
    let secondary_sub_model = Rc::new(slint::VecModel::<SubEntry>::default());
    app.set_secondary_subtitle_entries(secondary_sub_model.clone().into());
    app.set_volume(100.0);
    app.set_volume_max(100.0);
    app.set_is_muted(false);
    app.set_current_time("00:00".into());
    app.set_duration_time("00:00".into());
    app.set_app_version(env!("CARGO_PKG_VERSION").into());
    refresh_player_preferences(&app, &config_ctrl);
    refresh_recent_files(&app, &config_ctrl);
    #[cfg(feature = "ocr")]
    let ocr_controller = controller::ocr::OcrController::new();
    #[cfg(feature = "ocr")]
    if let Err(error) = &ocr_controller {
        eprintln!("OCR unavailable: {error}");
    }
    #[cfg(feature = "ocr")]
    let ocr_available = ocr_controller.is_ok();
    #[cfg(not(feature = "ocr"))]
    let ocr_available = false;
    let asr_session = Rc::new(RefCell::new(controller::asr::AsrController::new()));
    let asr_restart_pending = Rc::new(Cell::new(false));
    let closing = Rc::new(Cell::new(false));
    app.set_ocr_enabled(
        ocr_available && setting_enabled(
            &config_ctrl, raw::ocr::GROUP, raw::ocr::ENABLED,
            raw::ocr::ENABLED_DEFAULT,
        ),
    );
    app.set_asr_enabled(controller::asr::AVAILABLE &&
        setting_enabled(
            &config_ctrl, raw::asr::GROUP, raw::asr::ENABLED,
            raw::asr::ENABLED_DEFAULT,
        ),
    );
    // Creating a Slint Window at startup exposes it in Wayland's window list.
    // Keep Options and its Help window unconstructed until the user opens them.
    let settings_session: Rc<RefCell<Option<SettingsSession>>> = Rc::new(RefCell::new(None));
    let settings_on_open = settings_session.clone();
    let open_config = config_ctrl.clone();
    let open_dict = dict_ctrl.clone();
    let open_manager = dictionary_manager.clone();
    let open_dictionary_path = dictionary_database_path.clone();
    let open_cache = config_dir.clone();
    let open_anki = anki_json_path.clone();
    let open_app = app.as_weak();
    let open_asr_session = asr_session.clone();
    let open_subtitles = sub_ctrl.clone();
    let open_subtitle_model = sub_model.clone();
    let open_asr_restart = asr_restart_pending.clone();
    let open_settings_mpv = mpv.clone();
    let open_settings_coord = coordinator.clone();
    app.on_open_options(move || {
        let Some(app) = open_app.upgrade() else { return };
        if settings_on_open.borrow().is_none() {
            match create_settings_session(
                &app,
                open_config.clone(),
                open_dict.clone(),
                open_manager.clone(),
                open_dictionary_path.clone(),
                open_cache.clone(),
                open_anki.clone(),
                ocr_available,
                open_asr_session.clone(),
                open_subtitles.clone(),
                open_subtitle_model.clone(),
                open_asr_restart.clone(),
                open_settings_mpv.clone(),
                open_settings_coord.clone(),
            ) {
                Ok(session) => *settings_on_open.borrow_mut() = Some(session),
                Err(error) => {
                    app.set_status_message(format!("Could not open preferences: {error}").into());
                    return;
                }
            }
        }
        let settings = settings_on_open.borrow().as_ref().unwrap().window.clone_strong();
        settings.set_showing_audio_help(false);
        settings.set_connection_popup_open(false);
        let category = settings.get_active_category();
        refresh_category_settings(&settings, &open_config, category);
        refresh_custom_popup_css(&settings, &open_cache);
        if category == 4 {
            refresh_dictionary_settings(&settings, open_manager.as_deref(), &open_config);
        }
        if let Err(error) = settings.show() {
            app.set_status_message(format!("Could not open preferences: {error}").into());
        }
        settings.window().request_redraw();
        app.window().request_redraw();
    });

    let selected_profile_app = app.as_weak();
    let selected_profile_config = config_ctrl.clone();
    let selected_profile_path = anki_json_path.clone();
    let selected_profile_cache = config_dir.clone();
    app.on_select_anki_profile(move |name| {
        let Some(app) = selected_profile_app.upgrade() else { return };
        let result = selected_profile_config.edit_setting("anki/profile", &name)
            .and_then(|()| selected_profile_config.apply_settings(
                0, &selected_profile_cache, "memento.conf", &selected_profile_path,
            ));
        match result {
            Ok(()) => refresh_player_preferences(&app, &selected_profile_config),
            Err(error) => app.set_status_message(format!("Could not select Anki profile: {error}").into()),
        }
    });

    let current_media: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let media_generation = Rc::new(Cell::new(0u64));
    let shared_media_generation = Arc::new(AtomicU64::new(0));
    // Timestamp formatting callback
    app.on_format_timestamp(|sec| format_time(sec as f64).into());

    let hover_cursor = Rc::new(RefCell::new(None::<(String, i32)>));
    let hover_timer = Rc::new(slint::Timer::default());
    let hover_pending = Rc::new(RefCell::new(None::<String>));
    let hover_pause_owned = Rc::new(Cell::new(false));
    let hover_resume_timer = Rc::new(slint::Timer::default());
    let hover_player = mpv.clone();
    let hover_app = app.as_weak();
    let hover_owned = hover_pause_owned.clone();
    let resume_timer = hover_resume_timer.clone();
    app.on_subtitle_hover_changed(move |hovering| {
        let Some(app) = hover_app.upgrade() else { return };
        if hovering && app.get_hover_pause_enabled() {
            resume_timer.stop();
            if let Some(player) = hover_player.borrow().as_ref() {
                // Hover/layout callbacks must not synchronously wait on mpv.
                if app.get_is_playing() && !hover_owned.get() && player.pause().is_ok() {
                    hover_owned.set(true);
                }
            }
        } else if hover_owned.get() {
            let weak = hover_app.clone();
            let player = hover_player.clone();
            let owned = hover_owned.clone();
            resume_timer.start(slint::TimerMode::SingleShot, std::time::Duration::from_millis(70), move || {
                if weak.upgrade().is_some() && owned.replace(false) {
                    if let Some(player) = player.borrow().as_ref() { let _ = player.play(); }
                }
            });
        }
    });
    let lookup_epoch = Arc::new(AtomicU64::new(0));
    let key_mpv = mpv.clone();
    let key_closing = closing.clone();
    let key_hover_cursor = hover_cursor.clone();
    let shortcut_app = app.as_weak();
    app.set_custom_key_handler_installed(true);
    app.on_key_pressed(move |text, control, shift, alt, meta| {
        if key_closing.get() { return true; }
        let Some(app) = shortcut_app.upgrade() else { return false };
        // Bare modifiers (Shift/Ctrl/Alt/Super) must never dismiss YomitanPopup,
        // and pressing a lookup modifier while hovering a token triggers lookup.
        if is_bare_modifier_key(&text) {
            if app.global::<SubtitleOverlayBridge>().get_rich_enabled() {
                let (mc, ms, ma, mm) = bare_modifier_flags(&text);
                app.global::<SubtitleOverlayBridge>()
                    .invoke_stationary_modifier_changed(
                        control || mc,
                        shift || ms,
                        alt || ma,
                        meta || mm,
                    );
            } else if control || shift || alt || meta {
                let hovered = key_hover_cursor.borrow().clone();
                if let Some((word, index)) = hovered {
                    app.invoke_hover_word(word.into(), index, control, shift, alt, meta);
                }
            }
            return false;
        }
        let chord = keyboard_chord(&text, control, shift, alt, meta);
        let assigned = |binding: &slint::SharedString| {
            !binding.is_empty() && chord.eq_ignore_ascii_case(binding.as_str())
        };
        // Popup navigation and Anki card-add shortcuts operate on the open
        // YomitanPopup and must NOT dismiss it.
        if app.get_dict_open() {
            if assigned(&app.get_key_card_next()) {
                app.invoke_navigate_popup(1);
                return true;
            }
            if assigned(&app.get_key_card_prev()) {
                app.invoke_navigate_popup(-1);
                return true;
            }
            if assigned(&app.get_key_card_add()) && app.get_anki_enabled() {
                let idx = app.get_popup_focused_index().max(0);
                app.invoke_add_anki_note(idx);
                return true;
            }
        }
        // Any other keyboard input dismisses a definition, even when the key is
        // handled by the player or a child widget.
        let had_dict_open = app.get_dict_open();
        app.invoke_dismiss_definition();
        if assigned(&app.get_key_open_file()) {
            app.invoke_open_file();
        } else if assigned(&app.get_key_open_url()) {
            app.invoke_open_url();
        } else if assigned(&app.get_key_show_search()) || assigned(&app.get_key_card_add()) {
            app.set_dict_search_open(!app.get_dict_search_open());
        } else if assigned(&app.get_key_show_sub_list()) {
            app.set_sub_drawer_open(!app.get_sub_drawer_open());
        } else if assigned(&app.get_key_sub_list_find()) {
            app.set_sub_drawer_open(true);
            app.invoke_focus_subtitle_search();
        } else if assigned(&app.get_key_sub_list_find_next()) {
            app.set_sub_drawer_open(true);
            app.invoke_next_subtitle_search_match();
        } else if assigned(&app.get_key_sub_list_find_prev()) {
            app.set_sub_drawer_open(true);
            app.invoke_prev_subtitle_search_match();
        } else if assigned(&app.get_key_sub_list_copy()) {
            app.invoke_copy_selected_subtitles();
        } else if assigned(&app.get_key_osc_visibility()) {
            let next_mode = (app.get_osc_visibility_mode() + 1).rem_euclid(3);
            app.set_osc_visibility_mode(next_mode);
            match next_mode {
                0 => {
                    app.set_osc_visible(true);
                    app.set_status_message("OSC Visibility: Auto".into());
                }
                1 => {
                    app.set_osc_visible(true);
                    app.set_status_message("OSC Visibility: Visible".into());
                }
                _ => {
                    app.set_osc_visible(false);
                    app.set_status_message("OSC Visibility: Hidden".into());
                }
            }
        } else if assigned(&app.get_key_start_ocr()) {
            app.invoke_start_ocr();
        } else if assigned(&app.get_key_sub_auto_pause()) {
            app.invoke_auto_pause_changed(!app.get_auto_pause());
        } else if assigned(&app.get_key_sub_show()) {
            app.invoke_subtitles_visible_changed(!app.get_subtitles_visible());
        } else if assigned(&app.get_key_sub_furigana()) {
            app.invoke_furigana_visible_changed(!app.get_furigana_visible());
        } else if assigned(&app.get_key_sub_increase_size()) {
            app.invoke_adjust_subtitle_scale(0.005);
        } else if assigned(&app.get_key_sub_decrease_size()) {
            app.invoke_adjust_subtitle_scale(-0.005);
        } else if assigned(&app.get_key_sub_move_up()) {
            app.invoke_adjust_subtitle_offset(0.005);
        } else if assigned(&app.get_key_sub_move_down()) {
            app.invoke_adjust_subtitle_offset(-0.005);
        } else if !control && !alt && !meta && (text == "f" || text == "F") {
            app.invoke_toggle_fullscreen();
        } else if chord.eq_ignore_ascii_case("ctrl+,") || (control && (text == "," || text == "<")) {
            app.invoke_open_options();
        } else if text == " " && !(control || shift || alt || meta) {
            app.invoke_toggle_play();
        } else if text == "\u{001b}" || text == "Escape" {
            if app.get_update_dialog_open() {
                if app.get_update_dialog_opt_in() {
                    app.invoke_update_opt_in_responded(false);
                } else {
                    app.set_update_dialog_open(false);
                    app.invoke_update_dialog_dismissed();
                }
            } else if app.get_url_dialog_open() {
                app.set_url_dialog_open(false);
            } else if app.get_about_open() {
                app.set_about_open(false);
            } else if app.get_ocr_mode() {
                app.set_ocr_mode(false);
            } else if had_dict_open {
            } else if app.get_dict_search_open() {
                app.set_dict_search_open(false);
            } else if app.get_sub_drawer_open() {
                app.set_sub_drawer_open(false);
            } else if app.get_is_fullscreen() {
                app.invoke_toggle_fullscreen();
            } else {
                return false;
            }
        } else {
            if !control && !alt && (text == "q" || text == "Q") {
                app.invoke_quit_app();
                return true;
            }
            let key_base = match text.as_str() {
                "\u{F700}" => Some("UP"),
                "\u{F701}" => Some("DOWN"),
                "\u{F702}" => Some("LEFT"),
                "\u{F703}" => Some("RIGHT"),
                "\u{0008}" => Some("BS"),
                "\u{000a}" | "\r" => Some("ENTER"),
                "\u{0009}" | "\u{0019}" => Some("TAB"),
                "\u{007f}" | "\u{F728}" => Some("DEL"),
                "\u{F704}" => Some("F1"),
                "\u{F705}" => Some("F2"),
                "\u{F706}" => Some("F3"),
                "\u{F707}" => Some("F4"),
                "\u{F708}" => Some("F5"),
                "\u{F709}" => Some("F6"),
                "\u{F70A}" => Some("F7"),
                "\u{F70B}" => Some("F8"),
                "\u{F70C}" => Some("F9"),
                "\u{F70D}" => Some("F10"),
                "\u{F70E}" => Some("F11"),
                "\u{F70F}" => Some("F12"),
                _ => None,
            };
            let mpv_key = if let Some(base) = key_base {
                let mut name = String::new();
                if control { name.push_str("Ctrl+"); }
                if alt { name.push_str("Alt+"); }
                if shift { name.push_str("Shift+"); }
                if meta { name.push_str("Meta+"); }
                name.push_str(base);
                Some(name)
            } else if text.chars().count() == 1 {
                let c = text.chars().next().unwrap();
                if c.is_ascii_graphic() {
                    let mut name = String::new();
                    if control { name.push_str("Ctrl+"); }
                    if alt { name.push_str("Alt+"); }
                    if meta { name.push_str("Meta+"); }
                    name.push(c);
                    Some(name)
                } else {
                    None
                }
            } else {
                None
            };
            if let Some(key_name) = mpv_key {
                if let Some(player) = &*key_mpv.borrow() {
                    let _ = player.command(&["keypress", &key_name]);
                    return true;
                }
            }
            return false;
        }
        true
    });
    let key_rel_app = app.as_weak();
    let key_rel_closing = closing.clone();
    app.on_key_released(move |control, shift, alt, meta| {
        if key_rel_closing.get() { return; }
        let Some(app) = key_rel_app.upgrade() else { return };
        if app.global::<SubtitleOverlayBridge>().get_rich_enabled() {
            app.global::<SubtitleOverlayBridge>()
                .invoke_stationary_modifier_changed(control, shift, alt, meta);
        }
    });

    // The video surface is a Slint window, so mpv's fullscreen key cannot
    // fullscreen it. Keep the OSC timer and fullscreen cursor auto-hide in the UI layer.
    let last_pointer_activity = Rc::new(Cell::new(std::time::Instant::now()));
    let osc_timer = Rc::new(slint::Timer::default());
    let osc_timer_app = app.as_weak();
    let osc_timer_coord = coordinator.clone();
    let osc_timer_activity = last_pointer_activity.clone();
    osc_timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(app.get_osc_duration_ms() as u64),
        move || {
            let Some(app) = osc_timer_app.upgrade() else { return };
            let fullscreen = app.window().is_fullscreen() || app.get_is_fullscreen();
            if app.window().is_fullscreen() && !app.get_is_fullscreen() {
                app.set_is_fullscreen(true);
            }
            let mode = app.get_osc_visibility_mode();
            if mode == 1 {
                app.set_osc_visible(true);
            } else if mode == 2 {
                app.set_osc_visible(false);
            } else if app.get_duration() > 0.0
                && !app.get_menu_open()
                && !app.get_dict_open()
                && !app.get_osc_hovered()
            {
                app.set_osc_visible(false);
            }
            let overlay_interactive = app.get_menu_open()
                || app.get_dict_open()
                || app.get_osc_hovered()
                || app.get_ocr_mode()
                || app.get_about_open()
                || app.get_url_dialog_open()
                || app.get_update_dialog_open();
            if app.get_osc_mpv_cursor() {
                // Delegate cursor hiding to mpv's `cursor-autohide` policy instead of
                // Memento's OSC timer.
                let hide = osc_timer_coord
                    .try_borrow()
                    .ok()
                    .and_then(|guard| {
                        guard.as_ref().map(|coord| {
                            let st = coord.playback_state();
                            let elapsed_ms =
                                osc_timer_activity.get().elapsed().as_millis() as u64;
                            mpv_cursor_should_hide(
                                st.cursor_autohide,
                                st.cursor_autohide_fs_only,
                                fullscreen,
                                elapsed_ms,
                                overlay_interactive,
                            )
                        })
                    })
                    .unwrap_or(false);
                if app.get_cursor_hidden() != hide {
                    app.set_cursor_hidden(hide);
                }
            } else if fullscreen && !overlay_interactive {
                app.set_cursor_hidden(true);
            }
        },
    );
    let pointer_app = app.as_weak();
    let pointer_osc_timer = osc_timer.clone();
    let pointer_mpv = mpv.clone();
    let pointer_coord = coordinator.clone();
    let pointer_render_size = current_render_size.clone();
    let pointer_activity = last_pointer_activity.clone();
    let last_pointer = Rc::new(Cell::new(None::<(f32, f32)>));
    app.on_pointer_moved(move |x, y| {
        let Some(app) = pointer_app.upgrade() else { return };
        let (in_top, in_bottom) = subtitle_cursor_zones(y, app.get_video_height() as f32);
        if app.get_cursor_in_top_sub_zone() != in_top {
            app.set_cursor_in_top_sub_zone(in_top);
        }
        if app.get_cursor_in_bottom_sub_zone() != in_bottom {
            app.set_cursor_in_bottom_sub_zone(in_bottom);
        }
        if !app.get_dict_open()
            && !app.get_menu_open()
            && !app.get_ocr_mode()
            && !app.get_about_open()
            && !app.get_url_dialog_open()
            && !app.get_update_dialog_open()
        {
            if let (Ok(player_ref), Ok(mut coord_ref)) =
                (pointer_mpv.try_borrow(), pointer_coord.try_borrow_mut())
            {
                if let (Some(player), Some(coord)) = (player_ref.as_ref(), coord_ref.as_mut()) {
                    let (mx, my) = MouseInputForwarder::map_to_render_coords(
                        x,
                        y,
                        app.get_video_width() as f32,
                        app.get_video_height() as f32,
                        app.window().scale_factor(),
                        pointer_render_size.get(),
                    );
                    coord.mouse.send_move(player, mx, my);
                }
            }
        }
        let minimum = app.get_osc_min_move().max(0) as f32;
        let last = last_pointer.get();
        if last.is_some_and(|(old_x, old_y)| {
            (x - old_x).hypot(y - old_y) <= minimum
        }) {
            return;
        }
        last_pointer.set(Some((x, y)));
        pointer_activity.set(std::time::Instant::now());
        if app.get_cursor_hidden() {
            app.set_cursor_hidden(false);
        }
        if app.get_osc_visibility_mode() != 2 {
            app.set_osc_visible(true);
        }
        pointer_osc_timer.set_interval(std::time::Duration::from_millis(app.get_osc_duration_ms() as u64));
        pointer_osc_timer.restart();
    });
    let button_app = app.as_weak();
    let button_mpv = mpv.clone();
    let button_coord = coordinator.clone();
    let button_render_size = current_render_size.clone();
    app.on_video_pointer_button(move |x, y, qt_button, pressed, control, shift, alt, meta| {
        let Some(app) = button_app.upgrade() else { return };
        let (Ok(player_ref), Ok(mut coord_ref)) =
            (button_mpv.try_borrow(), button_coord.try_borrow_mut())
        else {
            return;
        };
        let (Some(player), Some(coord)) = (player_ref.as_ref(), coord_ref.as_mut()) else {
            return;
        };
        if qt_button <= 0 {
            coord.mouse.release_all(player);
            return;
        }
        let (mx, my) = MouseInputForwarder::map_to_render_coords(
            x,
            y,
            app.get_video_width() as f32,
            app.get_video_height() as f32,
            app.window().scale_factor(),
            button_render_size.get(),
        );
        let modifiers = MouseInputForwarder::modifiers_mask(control, shift, alt, meta);
        coord
            .mouse
            .send_button(player, mx, my, qt_button as u32, modifiers, pressed);
    });
    let wheel_app = app.as_weak();
    let wheel_mpv = mpv.clone();
    let wheel_coord = coordinator.clone();
    let wheel_render_size = current_render_size.clone();
    app.on_video_wheel(move |x, y, delta_x, delta_y| {
        let Some(app) = wheel_app.upgrade() else { return };
        let (Ok(player_ref), Ok(mut coord_ref)) =
            (wheel_mpv.try_borrow(), wheel_coord.try_borrow_mut())
        else {
            return;
        };
        let (Some(player), Some(coord)) = (player_ref.as_ref(), coord_ref.as_mut()) else {
            return;
        };
        let (mx, my) = MouseInputForwarder::map_to_render_coords(
            x,
            y,
            app.get_video_width() as f32,
            app.get_video_height() as f32,
            app.window().scale_factor(),
            wheel_render_size.get(),
        );
        coord.mouse.send_wheel(player, mx, my, delta_x, delta_y);
    });
    let activity_app = app.as_weak();
    let activity_osc_timer = osc_timer.clone();
    let activity_pointer_ts = last_pointer_activity.clone();
    app.on_osc_activity(move || {
        let Some(app) = activity_app.upgrade() else { return };
        activity_pointer_ts.set(std::time::Instant::now());
        if app.get_cursor_hidden() {
            app.set_cursor_hidden(false);
        }
        if app.get_osc_visibility_mode() != 2 {
            app.set_osc_visible(true);
        }
        activity_osc_timer.set_interval(std::time::Duration::from_millis(app.get_osc_duration_ms() as u64));
        activity_osc_timer.restart();
    });

    let cli_keep_paused = parsed_args.global_options.iter().any(|opt| {
        opt.name == "pause" && matches!(opt.value.trim(), "yes" | "true" | "1")
    });
    let pending_initial_loads: Rc<RefCell<Option<Vec<PlaylistLoad>>>> = Rc::new(RefCell::new(
        (!parsed_args.loads.is_empty()).then_some(parsed_args.loads),
    ));
    let trigger_initial_loads: Rc<dyn Fn()> = {
        let initial_app = app.as_weak();
        let initial_mpv = mpv.clone();
        let initial_coord = coordinator.clone();
        let initial_generation = media_generation.clone();
        let initial_shared_gen = shared_media_generation.clone();
        let initial_media = current_media.clone();
        let initial_config = config_ctrl.clone();
        let initial_cache = config_dir.clone();
        let initial_loads = pending_initial_loads.clone();
        Rc::new(move || {
            let Some(loads) = initial_loads.borrow_mut().take() else {
                return;
            };
            let app_weak = initial_app.clone();
            let mpv_rc = initial_mpv.clone();
            let coord_rc = initial_coord.clone();
            let gen_rc = initial_generation.clone();
            let shared_gen = initial_shared_gen.clone();
            let media_rc = initial_media.clone();
            let cfg_rc = initial_config.clone();
            let cache_path = initial_cache.clone();
            slint::Timer::single_shot(std::time::Duration::ZERO, move || {
                let Some(app) = app_weak.upgrade() else {
                    return;
                };
                let player_ref = mpv_rc.borrow();
                let Some(player) = player_ref.as_ref() else { return };
                match load_playlist_into(
                    &app,
                    player,
                    &loads,
                    &gen_rc,
                    &shared_gen,
                    Some(&coord_rc),
                    cli_keep_paused,
                ) {
                    Ok(Some(first_path)) => {
                        *media_rc.borrow_mut() = Some(first_path.clone());
                        if Path::new(&first_path).is_file() {
                            if let Err(error) =
                                record_recent_media(&app, &cfg_rc, &cache_path, &first_path)
                            {
                                eprintln!("Cannot save recent media: {error}");
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(error) => eprintln!("Cannot open CLI media: {error}"),
                }
            });
        })
    };

    thread_local! {
        static OPEN_PATHS_SINK: RefCell<Option<Rc<dyn Fn(Vec<PathBuf>)>>> = const { RefCell::new(None) };
    }

    let handle_open_or_drop_paths: Rc<dyn Fn(Vec<PathBuf>)> = {
        let weak_app = app.as_weak();
        let open_mpv = mpv.clone();
        let open_coord = coordinator.clone();
        let open_sub_ctrl = sub_ctrl.clone();
        let open_sub_model = sub_model.clone();
        let open_sec_sub_model = secondary_sub_model.clone();
        let open_current_media = current_media.clone();
        let open_config = config_ctrl.clone();
        let open_generation = media_generation.clone();
        let open_shared_gen = shared_media_generation.clone();
        let open_cache = config_dir.clone();
        Rc::new(move |paths: Vec<PathBuf>| {
            if paths.is_empty() {
                return;
            }
            let Some(app) = weak_app.upgrade() else { return };
            let mut media_paths = Vec::new();
            let mut subtitle_paths = Vec::new();
            for path in paths {
                if controller::player::is_subtitle_file(path.as_path()) {
                    subtitle_paths.push(path);
                } else {
                    media_paths.push(path);
                }
            }
            let player_ref = open_mpv.borrow();
            if !media_paths.is_empty() {
                let Some(player) = player_ref.as_ref() else {
                    app.set_status_message("Playback backend unavailable".into());
                    return;
                };
                if media_paths.len() == 1 {
                    let file_str = media_paths[0].to_string_lossy().into_owned();
                    match load_media_into(
                        &app,
                        player,
                        &file_str,
                        &open_generation,
                        &open_shared_gen,
                        Some(&open_coord),
                    ) {
                        Ok(()) => {
                            *open_current_media.borrow_mut() = Some(file_str.clone());
                            open_sub_ctrl.borrow_mut().clear();
                            open_sub_model.set_vec(Vec::new());
                            open_sec_sub_model.set_vec(Vec::new());
                            if let Err(error) =
                                record_recent_media(&app, &open_config, &open_cache, &file_str)
                            {
                                app.set_status_message(
                                    format!("Cannot save recent media: {error}").into(),
                                );
                            }
                        }
                        Err(error) => {
                            app.set_status_message(format!("Cannot open media: {error}").into());
                        }
                    }
                } else {
                    let loads: Vec<PlaylistLoad> = media_paths
                        .iter()
                        .map(|path| PlaylistLoad {
                            path: path.to_string_lossy().into_owned(),
                            options: Vec::new(),
                        })
                        .collect();
                    match load_playlist_into(
                        &app,
                        player,
                        &loads,
                        &open_generation,
                        &open_shared_gen,
                        Some(&open_coord),
                        false,
                    ) {
                        Ok(Some(first_path)) => {
                            *open_current_media.borrow_mut() = Some(first_path.clone());
                            open_sub_ctrl.borrow_mut().clear();
                            open_sub_model.set_vec(Vec::new());
                            open_sec_sub_model.set_vec(Vec::new());
                            if let Err(error) =
                                record_recent_media(&app, &open_config, &open_cache, &first_path)
                            {
                                app.set_status_message(
                                    format!("Cannot save recent media: {error}").into(),
                                );
                            }
                        }
                        Ok(None) => {}
                        Err(error) => {
                            app.set_status_message(format!("Cannot open playlist: {error}").into());
                        }
                    }
                }
            }
            for sub_path in subtitle_paths {
                let sub_str = sub_path.to_string_lossy().into_owned();
                let mut added_to_mpv = false;
                if let Some(player) = player_ref.as_ref() {
                    if app.get_duration() > 0.0 || !media_paths.is_empty() {
                        match player.command(&["sub-add", &sub_str]) {
                            Ok(()) => {
                                added_to_mpv = true;
                            }
                            Err(error) => {
                                app.set_status_message(
                                    format!("Could not add subtitles: {error}").into(),
                                );
                            }
                        }
                    }
                }
                if !added_to_mpv && sub_path.is_file() {
                    match parse_external_subtitles(&sub_path) {
                        Ok(cues) => {
                            let mut subtitles = open_sub_ctrl.borrow_mut();
                            let sid = if subtitles.primary_sid() > 0 {
                                subtitles.primary_sid()
                            } else {
                                subtitles.set_primary_sid(1);
                                app.set_primary_sid(1);
                                1
                            };
                            subtitles.set_external_cues(sid, &sub_str, cues);
                            refresh_subtitle_list(&app, &open_sub_model, &subtitles);
                        }
                        Err(error) => {
                            app.set_status_message(
                                format!("Could not add subtitles: {error}").into(),
                            );
                        }
                    }
                }
            }
        })
    };

    OPEN_PATHS_SINK.with(|slot| {
        *slot.borrow_mut() = Some(handle_open_or_drop_paths.clone());
    });

    let open_file_config = config_ctrl.clone();
    app.on_open_file(move || {
        let start_dir = resolve_config_file_open_dir(&open_file_config);
        memento_os::open_files_async(
            memento_os::FileDialogOptions::media_and_subtitles(start_dir),
            move |paths| {
                if paths.is_empty() {
                    return;
                }
                let _ = slint::invoke_from_event_loop(move || {
                    OPEN_PATHS_SINK.with(|slot| {
                        let cb = slot.borrow().clone();
                        if let Some(apply) = cb {
                            apply(paths);
                        }
                    });
                });
            },
        );
    });

    let drop_single = handle_open_or_drop_paths.clone();
    app.on_file_dropped(move |uri_or_path| {
        let paths = memento_os::parse_uri_list(uri_or_path.as_str());
        if !paths.is_empty() {
            drop_single(paths);
        }
    });

    let drop_multi = handle_open_or_drop_paths.clone();
    app.on_files_dropped(move |uris| {
        let mut paths = Vec::new();
        for i in 0..uris.row_count() {
            if let Some(entry) = uris.row_data(i) {
                for parsed in memento_os::parse_uri_list(entry.as_str()) {
                    if !paths.contains(&parsed) {
                        paths.push(parsed);
                    }
                }
            }
        }
        if !paths.is_empty() {
            drop_multi(paths);
        }
    });

    let recent_app = app.as_weak();
    let recent_mpv = mpv.clone();
    let recent_coord = coordinator.clone();
    let recent_subs = sub_ctrl.clone();
    let recent_model = sub_model.clone();
    let recent_sec_model = secondary_sub_model.clone();
    let recent_current = current_media.clone();
    let recent_generation = media_generation.clone();
    let recent_shared_gen = shared_media_generation.clone();
    let recent_config = config_ctrl.clone();
    let recent_cache = config_dir.clone();
    app.on_open_recent(move |path| {
        let Some(app) = recent_app.upgrade() else { return };
        let player = recent_mpv.borrow();
        let Some(player) = player.as_ref() else { return };
        match load_media_into(
            &app,
            player,
            &path,
            &recent_generation,
            &recent_shared_gen,
            Some(&recent_coord),
        ) {
            Ok(()) => {
                *recent_current.borrow_mut() = Some(path.to_string());
                recent_subs.borrow_mut().clear();
                recent_model.set_vec(Vec::new());
                recent_sec_model.set_vec(Vec::new());
                if let Err(error) =
                    record_recent_media(&app, &recent_config, &recent_cache, &path)
                {
                    app.set_status_message(format!("Cannot save recent media: {error}").into());
                }
            }
            Err(error) => app.set_status_message(format!("Cannot open media: {error}").into()),
        }
    });
    let clear_app = app.as_weak();
    let clear_config = config_ctrl.clone();
    let clear_cache = config_dir.clone();
    app.on_clear_recent(move || {
        recent_titles_map()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
        let result = clear_config.clear_recent_files()
            .and_then(|_| clear_config.save(&clear_cache, "memento.conf"));
        if let Some(app) = clear_app.upgrade() {
            match result {
                Ok(()) => refresh_recent_files(&app, &clear_config),
                Err(error) => app.set_status_message(format!("Cannot clear recents: {error}").into()),
            }
        }
    });

    // ── Transport Callbacks ─────────────────────────────────────────────────
    let play_mpv = mpv.clone();
    let play_hover_owned = hover_pause_owned.clone();
    let play_hover_resume = hover_resume_timer.clone();
    let play_app = app.as_weak();
    app.on_toggle_play(move || {
        play_hover_resume.stop();
        play_hover_owned.set(false);
        if let Some(app) = play_app.upgrade() {
            if app.get_pending_playback_generation() >= 0 {
                app.set_pending_playback_generation(-1);
                app.set_is_playing(false);
                return;
            }
        }
        if let Some(mpv_inst) = &*play_mpv.borrow() {
            let _ = mpv_inst.toggle_pause();
        }
    });

    let seek_mpv = mpv.clone();
    let seek_coord = coordinator.clone();
    let seek_asr = asr_session.clone();
    app.on_seek_to(move |sec| {
        if let Some(mpv_inst) = &*seek_mpv.borrow() {
            let target = (sec as f64).max(0.0);
            if mpv_inst.seek(target).is_ok() {
                if let (Ok(mut coord_ref), Ok(mut asr)) =
                    (seek_coord.try_borrow_mut(), seek_asr.try_borrow_mut())
                {
                    if let Some(coord) = coord_ref.as_mut() {
                        coord.notify_user_seek(&mut asr, Some(target), mpv_inst.get_duration());
                    }
                }
            }
        }
    });

    let rel_seek_mpv = mpv.clone();
    let rel_seek_coord = coordinator.clone();
    let rel_seek_asr = asr_session.clone();
    app.on_seek_relative(move |delta| {
        if let Some(mpv_inst) = &*rel_seek_mpv.borrow() {
            let dur = mpv_inst.get_duration();
            let target = (mpv_inst.get_position() + delta as f64).clamp(0.0, dur.max(0.0));
            if mpv_inst.seek_relative(delta as f64).is_ok() {
                if let (Ok(mut coord_ref), Ok(mut asr)) =
                    (rel_seek_coord.try_borrow_mut(), rel_seek_asr.try_borrow_mut())
                {
                    if let Some(coord) = coord_ref.as_mut() {
                        coord.notify_user_seek(&mut asr, Some(target), dur);
                    }
                }
            }
        }
    });

    let sub_seek_mpv = mpv.clone();
    let sub_seek_coord = coordinator.clone();
    let sub_seek_asr = asr_session.clone();
    app.on_sub_seek(move |direction| {
        if let Some(mpv_inst) = &*sub_seek_mpv.borrow() {
            if mpv_inst.subtitle_seek(direction, true).is_ok() {
                if let (Ok(mut coord_ref), Ok(mut asr)) =
                    (sub_seek_coord.try_borrow_mut(), sub_seek_asr.try_borrow_mut())
                {
                    if let Some(coord) = coord_ref.as_mut() {
                        coord.notify_user_seek(&mut asr, None, mpv_inst.get_duration());
                    }
                }
            }
        }
    });
    let prev_mpv = mpv.clone();
    app.on_playlist_prev(move || {
        if let Some(player) = &*prev_mpv.borrow() {
            if let Err(error) = player.playlist_prev() {
                eprintln!("Previous media: {error}");
            }
        }
    });
    let next_mpv = mpv.clone();
    app.on_playlist_next(move || {
        if let Some(player) = &*next_mpv.borrow() {
            if let Err(error) = player.playlist_next() {
                eprintln!("Next media: {error}");
            }
        }
    });

    let audio_mpv = mpv.clone();
    let audio_app = app.as_weak();
    app.on_select_audio_track(move |id| {
        if let Some(player) = &*audio_mpv.borrow() {
            match player.set_aid(id.into()) {
                Ok(()) => {
                    if let Some(app) = audio_app.upgrade() {
                        app.set_audio_id(id);
                    }
                }
                Err(error) => eprintln!("Cannot select audio track {id}: {error}"),
            }
        }
    });
    let primary_mpv = mpv.clone();
    let primary_coord = coordinator.clone();
    let primary_subs = sub_ctrl.clone();
    let primary_sub_model = sub_model.clone();
    let primary_app = app.as_weak();
    let primary_asr = asr_session.clone();
    let primary_asr_restart = asr_restart_pending.clone();
    app.on_select_subtitle_track(move |id| {
        if let Some(player) = &*primary_mpv.borrow() {
            match player.set_sid(id.into()) {
                Ok(()) => {
                    primary_asr.borrow_mut().suspend();
                    primary_asr_restart.set(false);
                    if let Ok(mut coord_ref) = primary_coord.try_borrow_mut() {
                        if let Some(coord) = coord_ref.as_mut() {
                            coord.on_manual_primary_sid(id.into());
                        }
                    }
                    let mut subtitles = primary_subs.borrow_mut();
                    subtitles.set_primary_sid(id.into());
                    if let Some(app) = primary_app.upgrade() {
                        refresh_subtitle_list(&app, &primary_sub_model, &subtitles);
                        app.set_asr_track_selected(false);
                        app.set_asr_running(false);
                        app.set_primary_sid(id);
                        clear_primary_subtitle(&app);
                    }
                }
                Err(error) => eprintln!("Cannot select primary subtitle track {id}: {error}"),
            }
        }
    });
    let secondary_mpv = mpv.clone();
    let secondary_subs = sub_ctrl.clone();
    let secondary_sec_model = secondary_sub_model.clone();
    let secondary_app = app.as_weak();
    app.on_select_secondary_subtitle_track(move |id| {
        if let Some(player) = &*secondary_mpv.borrow() {
            match player.set_secondary_sid(id.into()) {
                Ok(()) => {
                    let mut subtitles = secondary_subs.borrow_mut();
                    subtitles.set_secondary_sid(id.into());
                    if let Some(app) = secondary_app.upgrade() {
                        app.set_secondary_sid(id);
                        refresh_secondary_subtitle_list(&app, &secondary_sec_model, &subtitles);
                        if id <= 0 && app.get_subtitle_drawer_tab() == 1 {
                            app.set_subtitle_drawer_tab(0);
                        }
                    }
                }
                Err(error) => eprintln!("Cannot select secondary subtitle track {id}: {error}"),
            }
        } else {
            let mut subtitles = secondary_subs.borrow_mut();
            subtitles.set_secondary_sid(id.into());
            if let Some(app) = secondary_app.upgrade() {
                app.set_secondary_sid(id);
                refresh_secondary_subtitle_list(&app, &secondary_sec_model, &subtitles);
                if id <= 0 && app.get_subtitle_drawer_tab() == 1 {
                    app.set_subtitle_drawer_tab(0);
                }
            }
        }
    });

    let select_asr_app = app.as_weak();
    let select_asr_mpv = mpv.clone();
    let select_asr_coord = coordinator.clone();
    let select_asr_config = config_ctrl.clone();
    let select_asr_session = asr_session.clone();
    let select_asr_restart = asr_restart_pending.clone();
    let select_asr_sub_ctrl = sub_ctrl.clone();
    let select_asr_sub_model = sub_model.clone();
    app.on_select_asr(move || {
        let Some(app) = select_asr_app.upgrade() else { return };
        let Some(player) = &*select_asr_mpv.borrow() else {
            app.set_status_message("Playback backend unavailable".into());
            return;
        };
        let mut coord_ref = select_asr_coord.borrow_mut();
        let Some(coord) = coord_ref.as_mut() else { return };
        let mut asr = select_asr_session.borrow_mut();
        let mut subtitles = select_asr_sub_ctrl.borrow_mut();
        coord.toggle_or_start_asr(
            &app,
            player,
            &mut asr,
            &mut subtitles,
            &select_asr_sub_model,
            &select_asr_restart,
            || {
                let config_dir = memento_xdg_dir("XDG_CONFIG_HOME", ".config")
                    .unwrap_or_else(|| PathBuf::from("."));
                controller::asr::AsrSettings::from_config(&config_dir, &*select_asr_config.store())
            },
        );
    });
    let auto_app = app.as_weak();
    let auto_config = config_ctrl.clone();
    let auto_cache = config_dir.clone();
    let auto_anki = anki_json_path.clone();
    app.on_auto_pause_changed(move |enabled| {
        let result = auto_config
            .edit_setting("behavior/subtitle-pause", if enabled { "true" } else { "false" })
            .and_then(|()| auto_config.apply_settings(
                3, &auto_cache, "memento.conf", &auto_anki,
            ));
        match result {
            Ok(()) => {
                if let Some(app) = auto_app.upgrade() {
                    app.set_auto_pause(enabled);
                }
            }
            Err(error) => eprintln!("Cannot save subtitle auto-pause: {error}"),
        }
    });
    let scale_app = app.as_weak();
    let scale_config = config_ctrl.clone();
    let scale_cache = config_dir.clone();
    let scale_anki = anki_json_path.clone();
    app.on_adjust_subtitle_scale(move |delta| {
        if let Some(app) = scale_app.upgrade() {
            let new_scale = (app.get_subtitle_scale() + delta).clamp(0.001, 1.0);
            let result = scale_config
                .edit_setting("interface/sub-scale", &new_scale.to_string())
                .and_then(|()| scale_config.apply_settings(
                    5, &scale_cache, "memento.conf", &scale_anki,
                ));
            match result {
                Ok(()) => app.set_subtitle_scale(new_scale),
                Err(error) => eprintln!("Cannot save subtitle size: {error}"),
            }
        }
    });
    let offset_app = app.as_weak();
    let offset_config = config_ctrl.clone();
    let offset_cache = config_dir.clone();
    let offset_anki = anki_json_path.clone();
    app.on_adjust_subtitle_offset(move |delta| {
        if let Some(app) = offset_app.upgrade() {
            let new_offset = (app.get_subtitle_offset() + delta).clamp(0.0, 1.0);
            let result = offset_config
                .edit_setting("interface/sub-offset", &new_offset.to_string())
                .and_then(|()| offset_config.apply_settings(
                    5, &offset_cache, "memento.conf", &offset_anki,
                ));
            match result {
                Ok(()) => app.set_subtitle_offset(new_offset),
                Err(error) => eprintln!("Cannot save subtitle position: {error}"),
            }
        }
    });
    let visibility_app = app.as_weak();
    app.on_subtitles_visible_changed(move |visible| {
        if let Some(app) = visibility_app.upgrade() {
            app.set_subtitles_visible(visible);
        }
    });
    let furigana_app = app.as_weak();
    let furigana_config = config_ctrl.clone();
    let furigana_cache = config_dir.clone();
    let furigana_anki = anki_json_path.clone();
    app.on_furigana_visible_changed(move |visible| {
        let Some(app) = furigana_app.upgrade() else { return };
        let result = furigana_config
            .edit_setting("search/subtitle-furigana", if visible { "true" } else { "false" })
            .and_then(|()| furigana_config.apply_settings(9, &furigana_cache, "memento.conf", &furigana_anki));
        match result {
            Ok(()) => refresh_player_preferences(&app, &furigana_config),
            Err(error) => app.set_status_message(format!("Could not change furigana: {error}").into()),
        }
    });

    let vol_app = app.as_weak();
    let vol_mpv = mpv.clone();
    app.on_set_volume(move |vol| {
        let max_vol = vol_app
            .upgrade()
            .map(|app| app.get_volume_max().max(100.0))
            .unwrap_or(100.0);
        let clamped = vol.clamp(0.0, max_vol);
        if let Some(app) = vol_app.upgrade() {
            app.set_volume(clamped);
            if clamped > 0.0 && app.get_is_muted() {
                app.set_is_muted(false);
            }
        }
        if let Some(mpv_inst) = &*vol_mpv.borrow() {
            let _ = mpv_inst.set_volume(clamped as f64);
            if clamped > 0.0 {
                let _ = mpv_inst.command_async(&["set", "mute", "no"]);
            }
        }
    });

    // ── Menu Bar & Shortcut Callbacks ───────────────────────────────────────
    let weak_mute = app.as_weak();
    let mute_mpv = mpv.clone();
    app.on_toggle_mute(move || {
        if let Some(app) = weak_mute.upgrade() {
            let next_muted = !app.get_is_muted();
            app.set_is_muted(next_muted);
            if let Some(mpv_inst) = &*mute_mpv.borrow() {
                let _ = mpv_inst.command_async(&[
                    "set",
                    "mute",
                    if next_muted { "yes" } else { "no" },
                ]);
            }
        }
    });

    let weak_vup = app.as_weak();
    let vup_mpv = mpv.clone();
    app.on_volume_up(move || {
        if let Some(app) = weak_vup.upgrade() {
            let max_vol = app.get_volume_max().max(100.0);
            let new_vol = (app.get_volume() + 5.0).clamp(0.0, max_vol);
            app.set_volume(new_vol);
            if app.get_is_muted() {
                app.set_is_muted(false);
            }
            if let Some(mpv_inst) = &*vup_mpv.borrow() {
                let _ = mpv_inst.command_async(&["set", "mute", "no"]);
                let _ = mpv_inst.set_volume(new_vol as f64);
            }
        }
    });

    let weak_vdown = app.as_weak();
    let vdown_mpv = mpv.clone();
    app.on_volume_down(move || {
        if let Some(app) = weak_vdown.upgrade() {
            let max_vol = app.get_volume_max().max(100.0);
            let new_vol = (app.get_volume() - 5.0).clamp(0.0, max_vol);
            app.set_volume(new_vol);
            if let Some(mpv_inst) = &*vdown_mpv.borrow() {
                let _ = mpv_inst.set_volume(new_vol as f64);
            }
        }
    });

    let weak_url = app.as_weak();
    app.on_open_url(move || {
        let Some(app) = weak_url.upgrade() else { return };
        let clip = read_copied_text_or_clipboard();
        let trimmed = clip.trim();
        if trimmed.starts_with("http://")
            || trimmed.starts_with("https://")
            || trimmed.starts_with("rtsp://")
            || trimmed.starts_with("rtmp://")
            || trimmed.starts_with("mms://")
            || trimmed.starts_with("file://")
        {
            app.set_url_dialog_text(trimmed.into());
        }
        app.set_url_dialog_open(true);
    });

    let submit_url_app = app.as_weak();
    let url_mpv = mpv.clone();
    let url_coord = coordinator.clone();
    let url_current = current_media.clone();
    let url_subs = sub_ctrl.clone();
    let url_generation = media_generation.clone();
    let url_shared_gen = shared_media_generation.clone();
    let url_model = sub_model.clone();
    let url_sec_model = secondary_sub_model.clone();
    app.on_open_url_submitted(move |raw_url| {
        let Some(app) = submit_url_app.upgrade() else { return };
        app.set_url_dialog_open(false);
        let url = raw_url.trim().to_owned();
        if url.is_empty() {
            return;
        }
        if let Some(player) = &*url_mpv.borrow() {
            match load_media_into(
                &app,
                player,
                &url,
                &url_generation,
                &url_shared_gen,
                Some(&url_coord),
            ) {
                Ok(()) => {
                    *url_current.borrow_mut() = Some(url);
                    url_subs.borrow_mut().clear();
                    url_model.set_vec(Vec::new());
                    url_sec_model.set_vec(Vec::new());
                }
                Err(error) => app.set_status_message(format!("Cannot open URL: {error}").into()),
            }
        } else {
            *url_current.borrow_mut() = Some(url.clone());
            app.set_media_title(url.into());
        }
    });

    let reload_app = app.as_weak();
    let reload_mpv = mpv.clone();
    let reload_coord = coordinator.clone();
    let reload_current = current_media.clone();
    let reload_subs = sub_ctrl.clone();
    let reload_generation = media_generation.clone();
    let reload_shared_gen = shared_media_generation.clone();
    let reload_model = sub_model.clone();
    let reload_sec_model = secondary_sub_model.clone();
    app.on_reload_media(move || {
        let Some(app) = reload_app.upgrade() else { return };
        let current = reload_current.borrow().clone();
        let Some(path) = current.as_deref() else { return };
        if let Some(player) = &*reload_mpv.borrow() {
            match load_media_into(
                &app,
                player,
                path,
                &reload_generation,
                &reload_shared_gen,
                Some(&reload_coord),
            ) {
                Ok(()) => {
                    reload_subs.borrow_mut().clear();
                    reload_model.set_vec(Vec::new());
                    reload_sec_model.set_vec(Vec::new());
                }
                Err(error) => app.set_status_message(format!("Cannot reload media: {error}").into()),
            }
        }
    });

    let weak_close = app.as_weak();
    let close_mpv = mpv.clone();
    let close_coord = coordinator.clone();
    let close_sub_ctrl = sub_ctrl.clone();
    let close_sub_model = sub_model.clone();
    let close_sec_sub_model = secondary_sub_model.clone();
    let close_current = current_media.clone();
    let close_generation = media_generation.clone();
    let close_shared_gen = shared_media_generation.clone();
    let close_asr = asr_session.clone();
    let close_screensaver = screensaver.clone();
    app.on_close_media(move || {
        {
            let player_ref = close_mpv.borrow();
            if let Some(mpv_inst) = player_ref.as_ref() {
                if let Ok(mut coord_ref) = close_coord.try_borrow_mut() {
                    if let Some(coord) = coord_ref.as_mut() {
                        coord.remove_asr_track(Some(mpv_inst));
                    }
                }
                let _ = mpv_inst.command(&["stop"]);
            }
        }
        close_screensaver.borrow_mut().uninhibit();
        close_asr.borrow_mut().cancel();
        let next_gen = close_generation.get().wrapping_add(1);
        close_generation.set(next_gen);
        close_shared_gen.store(next_gen, Ordering::Release);
        *close_current.borrow_mut() = None;
        close_sub_ctrl.borrow_mut().clear();
        close_sub_model.set_vec(Vec::new());
        close_sec_sub_model.set_vec(Vec::new());
        if let Some(app) = weak_close.upgrade() {
            app.set_pending_playback_generation(-1);
            app.set_asr_track_selected(false);
            app.set_asr_running(false);
            app.set_media_title("".into());
            app.set_position(0.0);
            app.set_duration(0.0);
            app.set_is_playing(false);
            app.set_active_sub_text("".into());
            app.set_secondary_sub_text("".into());
            app.set_active_sub_words(Rc::new(VecModel::from(Vec::new())).into());
            app.set_active_sub_lines(Rc::new(VecModel::from(Vec::new())).into());
            app.set_chapters(Rc::new(VecModel::from(Vec::new())).into());
            app.set_chapter_marks(Rc::new(VecModel::from(Vec::new())).into());
            app.set_thumbnail_frame(slint::Image::default());
            app.set_hover_chapter_title("".into());
            app.set_selected_token_index(-1);
            app.set_current_time("00:00".into());
            app.set_duration_time("00:00".into());
        }
    });

    let quit_timer = Rc::new(slint::Timer::default());
    let quit_poll = quit_timer.clone();
    let quit_asr = asr_session.clone();
    let quit_restart = asr_restart_pending.clone();
    let quit_closing = closing.clone();
    let quit_player = mpv.clone();
    let quit_hover_resume = hover_resume_timer.clone();
    let quit_hover_owned = hover_pause_owned.clone();
    let quit_screensaver = screensaver.clone();
    let quit_app = app.as_weak();
    app.on_quit_app(move || {
        if quit_closing.replace(true) { return; }
        quit_screensaver.borrow_mut().uninhibit();
        quit_restart.set(false);
        quit_hover_resume.stop();
        quit_hover_owned.set(false);
        quit_asr.borrow_mut().begin_shutdown();
        if let Some(app) = quit_app.upgrade() {
            app.invoke_dismiss_definition();
            app.set_asr_track_selected(false);
            app.set_asr_running(false);
            app.set_pending_playback_generation(-1);
            app.set_status_message("Closing Memento — stopping speech recognition…".into());
        }
        if let Some(player) = quit_player.borrow().as_ref() {
            let _ = player.command_async(&["set", "pause", "yes"]);
        }
        if quit_asr.borrow_mut().try_finish_shutdown() {
            if let Err(error) = slint::quit_event_loop() {
                eprintln!("Could not close Memento: {error}");
            }
            return;
        }
        let asr = quit_asr.clone();
        quit_poll.start(slint::TimerMode::Repeated, std::time::Duration::from_millis(25), move || {
            if asr.borrow_mut().try_finish_shutdown() {
                if let Err(error) = slint::quit_event_loop() {
                    eprintln!("Could not close Memento: {error}");
                }
            }
        });
    });
    let close_app = app.as_weak();
    app.window().on_close_requested(move || {
        if let Some(app) = close_app.upgrade() { app.invoke_quit_app(); }
        slint::CloseRequestResponse::KeepWindowShown
    });

    let visibility_app = app.as_weak();
    app.on_toggle_subtitles(move || {
        if let Some(app) = visibility_app.upgrade() {
            app.invoke_subtitles_visible_changed(!app.get_subtitles_visible());
        }
    });

    let add_sub_config = config_ctrl.clone();
    app.on_add_subtitle(move || {
        let start_dir = resolve_config_file_open_dir(&add_sub_config);
        memento_os::open_files_async(
            memento_os::FileDialogOptions::subtitles_only(start_dir),
            move |paths| {
                if paths.is_empty() {
                    return;
                }
                let _ = slint::invoke_from_event_loop(move || {
                    OPEN_PATHS_SINK.with(|slot| {
                        let cb = slot.borrow().clone();
                        if let Some(apply) = cb {
                            apply(paths);
                        }
                    });
                });
            },
        );
    });

    let delay_mpv = mpv.clone();
    app.on_adjust_sub_delay(move |delta| {
        if let Some(mpv_inst) = &*delay_mpv.borrow() {
            let _ = mpv_inst.command(&["add", "sub-delay", &format!("{:.2}", delta)]);
        }
    });

    let ocr_mode_app = app.as_weak();
    app.on_start_ocr(move || {
        if let Some(app) = ocr_mode_app.upgrade() {
            if !app.get_ocr_enabled() {
                app.set_status_message("OCR is disabled or unavailable".into());
            } else if app.get_duration() <= 0.0 {
                app.set_status_message("Open a video before selecting text for OCR".into());
            } else {
                app.set_status_message("Drag a rectangle around text in the video".into());
                app.set_ocr_mode(true);
            }
        }
    });
    #[cfg(feature = "ocr")]
    {
    let ocr_region_app = app.as_weak();
    let ocr_region_mpv = mpv.clone();
    let ocr_region_config = config_ctrl.clone();
    app.on_ocr_region(move |x, y, width, height| {
        let Some(app) = ocr_region_app.upgrade() else { return };
        app.set_status_message("Recognizing selected text…".into());
        let Some(controller) = ocr_controller.as_ref().ok().cloned() else { return };
        let Some(player) = &*ocr_region_mpv.borrow() else { return };
        let capture = match player.request_screenshot(false) {
            Ok(capture) => capture,
            Err(error) => {
                app.set_status_message(format!("Cannot capture video: {error}").into());
                return;
            }
        };
        let defaults = controller::ocr::OcrSettings::default();
        let model = match ocr_region_config.effective_setting(raw::ocr::GROUP, raw::ocr::MODEL) {
            Some(Value::Text(model)) => model,
            _ => defaults.model,
        };
        let settings = controller::ocr::OcrSettings {
            enabled: setting_enabled(
                &ocr_region_config, raw::ocr::GROUP, raw::ocr::ENABLED,
                raw::ocr::ENABLED_DEFAULT,
            ),
            model,
            use_gpu: setting_enabled(
                &ocr_region_config, raw::ocr::GROUP, raw::ocr::USE_GPU,
                raw::ocr::USE_GPU_DEFAULT,
            ),
        };
        let response_app = ocr_region_app.clone();
        let viewport = controller::ocr::OcrViewport {
            width: app.get_video_width().into(),
            height: app.get_video_height().into(),
        };
        std::thread::spawn(move || {
            let frame = capture.wait().and_then(|raw| {
                controller::ocr::OcrFrame::from_rgba(raw.width, raw.height, raw.pixels)
                    .map_err(|error| error.to_string())
            });
            let frame = match frame {
                Ok(frame) => frame,
                Err(error) => {
                    let _ = response_app.upgrade_in_event_loop(move |app| {
                        app.set_status_message(format!("Cannot capture video: {error}").into());
                    });
                    return;
                }
            };
            let error_app = response_app.clone();
            let result = controller.read_region_async(
                frame,
                controller::ocr::OcrRegion {
                    x: x.into(), y: y.into(), width: width.into(), height: height.into(),
                },
                viewport,
                settings,
                move |recognized| {
                    let _ = response_app.upgrade_in_event_loop(move |app| match recognized {
                        Ok(text) => {
                            let text = text.trim();
                            if !text.is_empty() {
                                app.invoke_dismiss_definition();
                                app.set_dict_search_open(true);
                                if app.global::<PopupBridge>().get_rich_enabled() {
                                    app.global::<PopupBridge>()
                                        .invoke_panel_search_requested(text.into());
                                } else {
                                    app.set_dict_query(text.into());
                                    app.invoke_search_dictionary(text.into());
                                }
                                app.set_status_message(format!("OCR: {text}").into());
                            } else {
                                app.set_status_message("No text recognized in selected region".into());
                            }
                        }
                        Err(error) => app.set_status_message(error.to_string().into()),
                    });
                },
            );
            if let Err(error) = result {
                let _ = error_app.upgrade_in_event_loop(move |app| {
                    app.set_status_message(error.to_string().into());
                });
            }
        });
    });
    }


    let update_app = app.as_weak();
    app.on_check_updates(move || {
        spawn_update_check(update_app.clone(), false);
    });

    let opt_in_app = app.as_weak();
    let opt_in_config = config_ctrl.clone();
    let opt_in_cache = config_dir.clone();
    let opt_in_settings = settings_session.clone();
    app.on_update_opt_in_responded(move |accepted| {
        let Some(app) = opt_in_app.upgrade() else { return };
        app.set_update_dialog_open(false);
        app.set_update_dialog_opt_in(false);
        let opt_in_key = format!("{}/{}", raw::internal::GROUP, raw::internal::AUTO_UPDATE_OPT_IN_SHOWN);
        let auto_check_key = format!("{}/{}", raw::application::GROUP, raw::application::AUTO_UPDATE_CHECK);
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
                    app.set_status_message(format!("Could not open configuration directory: {error}").into());
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

    // ── Timeline Hover Preview (Chapter Label + Background MpvThumbnail Worker) ──
    let (thumb_tx, thumb_rx) = std::sync::mpsc::channel::<(String, f64)>();
    let thumb_worker_app = app.as_weak();
    std::thread::spawn(move || {
        let mut thumb_mpv: Option<(MpvInstance, memento_player::mpv::MpvSoftwareRenderer)> = None;
        let mut loaded_path = String::new();
        while let Ok(mut req) = thumb_rx.recv() {
            while let Ok(newer) = thumb_rx.try_recv() {
                req = newer;
            }
            let (path, time_sec) = req;
            if path.is_empty() || !Path::new(&path).is_file() {
                continue;
            }
            if thumb_mpv.is_none() {
                let opts = [
                    memento_player::CliOption { name: "config".into(), value: "no".into() },
                    memento_player::CliOption { name: "aid".into(), value: "no".into() },
                    memento_player::CliOption { name: "sid".into(), value: "no".into() },
                    memento_player::CliOption { name: "secondary-sid".into(), value: "no".into() },
                    memento_player::CliOption { name: "osc".into(), value: "no".into() },
                    memento_player::CliOption { name: "pause".into(), value: "yes".into() },
                    memento_player::CliOption { name: "ytdl".into(), value: "no".into() },
                    memento_player::CliOption { name: "terminal".into(), value: "no".into() },
                    memento_player::CliOption { name: "hwdec".into(), value: "no".into() },
                    memento_player::CliOption { name: "hr-seek".into(), value: "no".into() },
                    memento_player::CliOption { name: "load-scripts".into(), value: "no".into() },
                ];
                if let Ok(inst) = MpvInstance::new_with_options(None, &opts) {
                    let renderer = inst.software_renderer();
                    thumb_mpv = Some((inst, renderer));
                }
            }
            let Some((inst, renderer)) = thumb_mpv.as_ref() else {
                continue;
            };
            if loaded_path != path {
                if inst.load_file(&path).is_err() {
                    continue;
                }
                loaded_path = path.clone();
                let deadline = std::time::Instant::now() + std::time::Duration::from_millis(160);
                while std::time::Instant::now() < deadline {
                    if inst.take_render_update() {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
            let _ = inst.command(&["seek", &format!("{:.3}", time_sec.max(0.0)), "absolute+keyframes"]);
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(80);
            while std::time::Instant::now() < deadline {
                if inst.take_render_update() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(8));
            }
            let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(160, 90);
            if unsafe { renderer.render_rgba(160, 90, buffer.make_mut_bytes()) }.is_ok() {
                let _ = thumb_worker_app.upgrade_in_event_loop(move |app| {
                    app.set_thumbnail_frame(slint::Image::from_rgba8(buffer));
                });
            }
        }
    });

    let hover_timeline_app = app.as_weak();
    let hover_timeline_media = current_media.clone();
    let hover_thumb_tx = thumb_tx.clone();
    app.on_timeline_hover_moved(move |time_sec| {
        let Some(app) = hover_timeline_app.upgrade() else { return };
        let chapter_title = chapter_title_at_time(&app, time_sec);
        if app.get_hover_chapter_title().as_str() != chapter_title {
            app.set_hover_chapter_title(chapter_title.into());
        }
        if app.get_preview_thumbnails_enabled() {
            if let Some(path) = hover_timeline_media.borrow().clone() {
                let _ = hover_thumb_tx.send((path, time_sec as f64));
            }
        }
    });

    let weak_fs = app.as_weak();
    app.on_toggle_fullscreen(move || {
        if let Some(app) = weak_fs.upgrade() {
            let next = !(app.window().is_fullscreen() || app.get_is_fullscreen());
            app.window().set_fullscreen(next);
            app.set_is_fullscreen(next);
            app.set_cursor_hidden(false);
            if app.get_osc_visibility_mode() != 2 {
                app.set_osc_visible(true);
            }
            app.invoke_osc_activity();
        }
    });

    install_subtitle_selection(&app, sub_ctrl.clone(), sub_model.clone());

    let ts_app = app.as_weak();
    let ts_config = config_ctrl.clone();
    let ts_cache = config_dir.clone();
    let ts_settings = settings_session.clone();
    app.on_sub_list_show_timestamps_toggled(move |show| {
        let Some(app) = ts_app.upgrade() else { return };
        app.set_sub_list_show_timestamps(show);
        let key = format!(
            "{}/{}",
            raw::interface::GROUP,
            raw::interface::subtitle_list::TIMESTAMPS
        );
        let val = if show { "true" } else { "false" };
        if let Err(error) = ts_config.persist_single_setting(&key, val, &ts_cache, "memento.conf") {
            app.set_status_message(format!("Could not save timestamps preference: {error}").into());
        }
        if let Some(session) = ts_settings.borrow().as_ref() {
            if session.window.get_active_category() == 5 {
                refresh_category_settings(&session.window, &ts_config, 5);
            }
        }
    });

    let ws_app = app.as_weak();
    let ws_config = config_ctrl.clone();
    let ws_cache = config_dir.clone();
    let ws_settings = settings_session.clone();
    app.on_subtitle_search_ignore_whitespace_changed(move |enabled| {
        let Some(app) = ws_app.upgrade() else { return };
        app.set_subtitle_search_ignore_whitespace(enabled);
        let key = format!(
            "{}/{}",
            raw::subtitle_list::GROUP,
            raw::subtitle_list::IGNORE_WHITESPACE
        );
        let val = if enabled { "true" } else { "false" };
        if let Err(error) = ws_config.persist_single_setting(&key, val, &ws_cache, "memento.conf") {
            app.set_status_message(
                format!("Could not save ignore-whitespace preference: {error}").into(),
            );
        }
        if let Some(session) = ws_settings.borrow().as_ref() {
            if session.window.get_active_category() == 5 {
                refresh_category_settings(&session.window, &ws_config, 5);
            }
        }
        let query = app.get_subtitle_search_query();
        if !query.trim().is_empty() {
            app.invoke_search_subtitles(query);
        }
    });

    let seek_app = app.as_weak();
    let seek_config = config_ctrl.clone();
    let seek_cache = config_dir.clone();
    let seek_settings = settings_session.clone();
    app.on_subtitle_search_auto_seek_changed(move |enabled| {
        let Some(app) = seek_app.upgrade() else { return };
        app.set_subtitle_search_auto_seek(enabled);
        let key = format!(
            "{}/{}",
            raw::subtitle_list::GROUP,
            raw::subtitle_list::AUTO_SEEK
        );
        let val = if enabled { "true" } else { "false" };
        if let Err(error) =
            seek_config.persist_single_setting(&key, val, &seek_cache, "memento.conf")
        {
            app.set_status_message(format!("Could not save auto-seek preference: {error}").into());
        }
        if let Some(session) = seek_settings.borrow().as_ref() {
            if session.window.get_active_category() == 5 {
                refresh_category_settings(&session.window, &seek_config, 5);
            }
        }
        if enabled {
            let focused = app.get_subtitle_search_focused_row();
            if focused >= 0 {
                auto_seek_to_matched_row(&app, focused as usize);
            }
        }
    });

    // ── Dictionary Lookup ──────────────────────────────────────────────────
    let popup_audio_cache: Arc<Mutex<PopupAudioState>> = Arc::new(Mutex::new(PopupAudioState::default()));
    let (lookup_tx, lookup_rx) = std::sync::mpsc::channel::<WordLookupRequest>();
    let worker_epoch = lookup_epoch.clone();
    let lookup_path = dictionary_database_path.clone();
    let worker_app = app.as_weak();
    std::thread::spawn(move || {
        let mut database_signature = controller::dict::db_file_fingerprint(&lookup_path);
        let mut dictionary = DictController::open(&lookup_path).unwrap_or_else(|error| {
            eprintln!("Dictionary lookup unavailable: {error}");
            DictController::new()
        });
        while let Ok(mut request) = lookup_rx.recv() {
            // Hover can produce several searches while a slow dictionary is
            // busy. Only the most recent query is useful to the popup.
            while let Ok(newer) = lookup_rx.try_recv() { request = newer; }
            if request.generation != worker_epoch.load(Ordering::Acquire) { continue; }
            let signature = controller::dict::db_file_fingerprint(&lookup_path);
            if signature != database_signature {
                dictionary = DictController::open(&lookup_path).unwrap_or_else(|error| {
                    eprintln!("Dictionary lookup unavailable: {error}");
                    DictController::new()
                });
                database_signature = signature;
            }
            dictionary.set_order(&request.order);
            dictionary.set_exact_enabled(request.exact_enabled);
            dictionary.set_deconj_enabled(request.deconj_enabled);
            dictionary.set_mecab_enabled(request.mecab_enabled);
            dictionary.set_search_text_filters(
                &request.remove_regex,
                request.replace_lines,
                &request.replace_with,
            );
            let hits = dictionary.search(&request.query).to_vec();
            let error = dictionary.take_search_error();
            let database_open = dictionary.database_path_is_open();
            let matched_units = dictionary.last_match_units();
            let headword = hits.first().map(|hit| hit.expression.clone()).unwrap_or_default();
            let reading = hits.first().map(|hit| hit.reading.clone()).unwrap_or_default();
            let meta_headword = headword.clone();
            let meta_reading = reading.clone();
            let mut pitch_terms = Vec::new();
            for hit in &hits {
                if !hit.is_kanji && !pitch_terms.iter().any(|(expression, reading): &(String, String)|
                    expression == &hit.expression && reading == &hit.reading) {
                    pitch_terms.push((hit.expression.clone(), hit.reading.clone()));
                }
            }
            let result = WordLookupResult {
                hits, error, database_open, matched_units, headword, reading,
                pitch: String::new(), freq: String::new(), jpdb: String::new(),
            };
            if request.generation != worker_epoch.load(Ordering::Acquire) { continue; }
            let generation = request.generation;
            let query = request.query;
            let source_subtitle = request.source_subtitle;
            let excluded = request.excluded;
            let meta_subtitle = source_subtitle.clone();
            let event_epoch = worker_epoch.clone();
            let _ = worker_app.upgrade_in_event_loop(move |app| {
                if generation == event_epoch.load(Ordering::Acquire)
                    && app.get_active_sub_text().as_str() == source_subtitle
                {
                    display_word_lookup(&app, result, &query, &excluded);
                }
            });
            if !meta_headword.is_empty() && generation == worker_epoch.load(Ordering::Acquire) {
                let (pitch, freq, jpdb) = dictionary.get_term_meta(&meta_headword, &meta_reading);
                let mut graphs = Vec::with_capacity(pitch_terms.len());
                for (expression, reading) in pitch_terms {
                    if generation != worker_epoch.load(Ordering::Acquire) { break; }
                    let term_graphs = dictionary.get_pitch_graphs(&expression, &reading);
                    let frequencies = dictionary.get_frequencies(&expression, &reading);
                    graphs.push((expression, reading, term_graphs, frequencies));
                }
                if generation != worker_epoch.load(Ordering::Acquire) { continue; }
                let event_epoch = worker_epoch.clone();
                let _ = worker_app.upgrade_in_event_loop(move |app| {
                    if generation == event_epoch.load(Ordering::Acquire)
                        && app.get_active_sub_text().as_str() == meta_subtitle
                    {
                        app.set_dict_pitch_accent(pitch.into());
                        app.set_dict_freq_rank(freq.into());
                        app.set_dict_jpdb_rank(jpdb.into());
                        for (expression, reading, graph_rows, frequencies) in graphs {
                            set_popup_term_metadata(&app, &expression, &reading, graph_rows, frequencies);
                        }
                    }
                });
            }
        }
    });
    let lookup_config = config_ctrl.clone();
    let lookup_sender = lookup_tx.clone();
    let lookup_generation = lookup_epoch.clone();
    let lookup_app = app.as_weak();
    let lookup_audio_cache = popup_audio_cache.clone();
    app.on_lookup_word(move |word| {
        let Some(app) = lookup_app.upgrade() else { return };
        set_popup_audio_sources(&app, &lookup_audio_cache, "", "", lookup_config.audio_source_entries());
        let order = match lookup_config.effective_setting(raw::dictionary::GROUP, raw::dictionary::ORDER) {
            Some(Value::Ints(order)) => order,
            _ => Vec::new(),
        };
        let exact_enabled = exact_matcher_enabled(&lookup_config);
        let deconj_enabled = deconj_matcher_enabled(&lookup_config);
        let mecab_enabled = mecab_matcher_enabled(&lookup_config);
        let (remove_regex, replace_lines, replace_with) = search_text_filters(&lookup_config);
        let generation = lookup_generation.fetch_add(1, Ordering::AcqRel) + 1;
        let _ = lookup_sender.send(WordLookupRequest {
            generation,
            query: word.to_string(),
            order,
            source_subtitle: app.get_active_sub_text().to_string(),
            excluded: lookup_config.anki_excluded_glossaries(),
            exact_enabled,
            deconj_enabled,
            mecab_enabled,
            remove_regex,
            replace_lines,
            replace_with,
        });
    });
    let glossary_link_app = app.as_weak();
    app.on_open_glossary_link(move |href| {
        let Some(app) = glossary_link_app.upgrade() else { return };
        if let Some(query_string) = href.strip_prefix('?') {
            if let Some((_, query)) = url::form_urlencoded::parse(query_string.as_bytes())
                .find(|(key, _)| key == "query") {
                app.invoke_lookup_word(query.into_owned().into());
            }
        } else if url::Url::parse(href.as_str()).is_ok_and(|url| matches!(url.scheme(), "http" | "https")) {
            let link = href.to_string();
            let weak = glossary_link_app.clone();
            std::thread::spawn(move || {
                if let Err(error) = webbrowser::open(&link) {
                    let _ = weak.upgrade_in_event_loop(move |app| {
                        app.set_status_message(format!("Could not open dictionary link: {error}").into());
                    });
                }
            });
        }
    });
    let hover_app = app.as_weak();
    let hover_config = config_ctrl.clone();
    let hover_timer_callback = hover_timer.clone();
    let hover_pending_callback = hover_pending.clone();
    let hover_cursor_callback = hover_cursor.clone();
    let hover_generation = lookup_epoch.clone();
    app.on_hover_word(move |word, index, control, shift, alt, meta| {
        let Some(app) = hover_app.upgrade() else { return };
        hover_timer_callback.stop();
        if word.is_empty() {
            hover_cursor_callback.borrow_mut().take();
            hover_pending_callback.borrow_mut().take();
            hover_generation.fetch_add(1, Ordering::AcqRel);
            return;
        }
        hover_cursor_callback.replace(Some((word.to_string(), index)));

        let method = match hover_config.effective_setting(raw::search::GROUP, raw::search::METHOD) {
            Some(Value::Int(value)) => i32::try_from(value)
                .unwrap_or(raw::search::METHOD_DEFAULT as i32),
            _ => raw::search::METHOD_DEFAULT as i32,
        };
        let modifier = match hover_config.effective_setting(raw::search::GROUP, raw::search::MODIFIER) {
            Some(Value::Int(value)) => i32::try_from(value)
                .unwrap_or(raw::search::MODIFIER_DEFAULT as i32),
            _ => raw::search::MODIFIER_DEFAULT as i32,
        };
        let modifier_held = match modifier {
            0 => alt,
            1 => control,
            2 => shift,
            3 => meta,
            _ => false,
        };
        if method == 1 && !modifier_held {
            hover_pending_callback.borrow_mut().take();
            hover_generation.fetch_add(1, Ordering::AcqRel);
            return;
        }

        // If a word popup is currently open and an index was explicitly clicked, don't overwrite on hover
        if app.get_dict_open() && app.get_selected_token_index() >= 0 && !modifier_held {
            return;
        }

        hover_generation.fetch_add(1, Ordering::AcqRel);

        let delay_ms = match hover_config.effective_setting(raw::search::GROUP, raw::search::DELAY) {
            Some(Value::Int(value)) => value.max(0) as u64,
            _ => raw::search::DELAY_DEFAULT as u64,
        };
        // A held lookup modifier is an explicit request; the ordinary hover
        // delay makes Shift lookup feel unresponsive.
        let delay_ms = if modifier_held { delay_ms.min(50) } else { delay_ms };
        hover_pending_callback.replace(Some(word.to_string()));
        app.set_selected_token_index(index);
        let pending = hover_pending_callback.clone();
        let weak = app.as_weak();
        hover_timer_callback.start(
            slint::TimerMode::SingleShot,
            std::time::Duration::from_millis(delay_ms),
            move || {
                let Some(word) = pending.borrow_mut().take() else { return };
                let Some(app) = weak.upgrade() else { return };
                let index = app.get_selected_token_index();
                if index >= 0 && app.get_active_sub_words().row_data(index as usize)
                    .is_some_and(|token| token.query.as_str() == word)
                {
                    app.set_popup_offset_x(app.get_pending_popup_offset_x());
                    app.invoke_lookup_word(word.into());
                }
            },
        );
    });
    let search_dict_app = app.as_weak();
    let search_dict_config = config_ctrl.clone();
    let search_dict_ctrl = dict_ctrl.clone();
    let search_audio_cache = popup_audio_cache.clone();
    app.on_search_dictionary(move |query| {
        let Some(app) = search_dict_app.upgrade() else { return };
        if app.global::<PopupBridge>().get_rich_enabled() {
            app.global::<PopupBridge>().invoke_panel_search_requested(query);
            return;
        }
        set_popup_audio_sources(&app, &search_audio_cache, "", "", search_dict_config.audio_source_entries());
        let hits = if query.trim().is_empty() {
            Vec::new()
        } else {
            let Ok(mut dictionary) = search_dict_ctrl.try_borrow_mut() else { return };
            apply_dictionary_order(&search_dict_config, &mut dictionary);
            let hits = dictionary.search(&query).to_vec();
            let error = dictionary.take_search_error();
            let database_open = dictionary.database_path_is_open();
            drop(dictionary);
            if let Some(error) = error {
                app.set_status_message(format!("Dictionary search failed: {error}").into());
            } else if hits.is_empty() && !database_open && !query.trim().is_empty() {
                app.set_status_message("Dictionary database is unavailable".into());
            }
            hits
        };
        app.invoke_dismiss_definition();
        set_dictionary_rows(&app, dictionary_result_rows(&hits, &search_dict_config.anki_excluded_glossaries()));
        app.invoke_reset_definition_scroll();
        if let Some(first_term) = hits.iter().find(|hit| !hit.is_kanji) {
            maybe_auto_play_first_term_audio(&app, &first_term.expression, &first_term.reading);
        }
    });
    let kanji_app = app.as_weak();
    let kanji_dict = dict_ctrl.clone();
    let kanji_config = config_ctrl.clone();
    let kanji_audio_cache = popup_audio_cache.clone();
    app.on_lookup_kanji(move |word| {
        let Some(app) = kanji_app.upgrade() else { return };
        set_popup_audio_sources(&app, &kanji_audio_cache, "", "", kanji_config.audio_source_entries());
        let Some(character) = word.chars().find(|character| {
            ('\u{3400}'..='\u{9fff}').contains(character)
                || ('\u{f900}'..='\u{faff}').contains(character)
                || ('\u{20000}'..='\u{3134f}').contains(character)
        }) else {
            app.set_status_message("No kanji in selected word".into());
            return;
        };
        let (hits, error, database_open, headword, reading, pitch, freq, jpdb) = {
            let Ok(mut dictionary) = kanji_dict.try_borrow_mut() else { return };
            let hits = dictionary.search_kanji(&character.to_string()).to_vec();
            let error = dictionary.take_search_error();
            let database_open = dictionary.database_path_is_open();
            let headword = hits.first().map(|hit| hit.expression.clone()).unwrap_or_default();
            let reading = hits.first().map(|hit| hit.reading.clone()).unwrap_or_default();
            let (pitch, freq, jpdb) = if !headword.is_empty() {
                dictionary.get_term_meta(&headword, &reading)
            } else {
                (String::new(), String::new(), String::new())
            };
            (hits, error, database_open, headword, reading, pitch, freq, jpdb)
        };
        if let Some(error) = error {
            app.set_status_message(format!("Kanji lookup failed: {error}").into());
        } else if hits.is_empty() && !database_open {
            app.set_status_message("Dictionary database is unavailable".into());
        }
        app.set_dict_open(false);
        set_dictionary_rows(&app, dictionary_result_rows(&hits, &kanji_config.anki_excluded_glossaries()));
        app.invoke_reset_definition_scroll();
        app.set_dict_query(headword.into());
        app.set_dict_reading(reading.into());
        app.set_dict_pitch_accent(pitch.into());
        app.set_dict_freq_rank(freq.into());
        app.set_dict_jpdb_rank(jpdb.into());
        app.set_dict_open(!hits.is_empty());
    });
    let audio_player: Rc<RefCell<Option<controller::audio::AudioController>>> =
        Rc::new(RefCell::new(None));
    let source_menu_app = app.as_weak();
    let source_menu_config = config_ctrl.clone();
    let source_menu_cache = popup_audio_cache.clone();
    app.on_popup_audio_sources_requested(move |expression, reading| {
        let Some(app) = source_menu_app.upgrade() else { return };
        let expression = expression.to_string();
        let reading = reading.to_string();
        let already_resolved = {
            let state = source_menu_cache.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            state.expression == expression && state.reading == reading
                && (!state.sources.is_empty() || app.get_popup_audio_sources_loading())
        };
        if already_resolved { return; }
        let sources = source_menu_config.audio_source_entries();
        // Wait for expanded JSON recordings before exposing selectable indices.
        // A source can expand to several recordings, so configured-source
        // indices would point at the wrong file after resolution.
        set_popup_audio_sources(&app, &source_menu_cache, &expression, &reading, Vec::new());
        app.set_popup_audio_sources_loading(true);
        let serial = source_menu_cache.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).serial;
        let weak = source_menu_app.clone();
        let cache = source_menu_cache.clone();
        std::thread::spawn(move || {
            let files = controller::audio::enumerate_word_audio_sources(&expression, &reading, &sources);
            let _ = weak.upgrade_in_event_loop(move |app| {
                let still_current = {
                    let state = cache.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                    state.serial == serial && state.expression == expression && state.reading == reading
                };
                if still_current {
                    set_popup_audio_sources(&app, &cache, &expression, &reading, files);
                }
            });
        });
    });
    let audio_app = app.as_weak();
    let audio_config = config_ctrl.clone();
    let playback_audio_cache = popup_audio_cache.clone();
    app.on_play_audio_for_word(move |expression, reading| {
        let Some(app) = audio_app.upgrade() else { return };
        let state = playback_audio_cache.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let matching = state.expression == expression.as_str() && state.reading == reading.as_str();
        let mut sources = if matching { state.sources.clone() } else { Vec::new() };
        drop(state);
        if sources.is_empty() { sources = audio_config.audio_source_entries(); }
        let selected_source = if matching { app.get_popup_selected_audio_source() } else { -1 };
        if selected_source >= 0 {
            sources = sources.into_iter().nth(selected_source as usize).into_iter().collect();
        }
        if sources.is_empty() {
            app.set_status_message("No pronunciation audio source is configured".into());
            return;
        }
        let mut player = audio_player.borrow_mut();
        if player.is_none() {
            match controller::audio::AudioController::new() {
                Ok(controller) => *player = Some(controller),
                Err(error) => {
                    app.set_status_message(format!("Pronunciation audio is unavailable: {error}").into());
                    return;
                }
            }
        }
        let weak = app.as_weak();
        let result = player.as_ref().unwrap().play_word(
            expression.to_string(),
            reading.to_string(),
            sources,
            move |result| {
                if let Err(error) = result {
                    let _ = weak.upgrade_in_event_loop(move |app| {
                        app.set_status_message(format!("Could not play pronunciation: {error}").into());
                    });
                }
            },
        );
        if let Err(error) = result {
            app.set_status_message(format!("Could not start pronunciation: {error}").into());
        }
    });
    ui::install_popup_navigation(&app);
    ui::install_rich_ui_bridges(&app, &dictionary_database_path, &config_dir, &config_ctrl);
    ui::bind_rich_bridge_lookup_epoch(&app, lookup_epoch.clone());
    // INTEGRATION NOTE (popup-features -> PopupBridge.first-term-ready):
    // Connect `app.global::<PopupBridge>().on_first_term_ready(...)` here to
    // `maybe_auto_play_first_term_audio(&app, expression.as_str(), reading.as_str())`.

    let note_app = app.as_weak();
    let note_config = config_ctrl.clone();
    let note_audio_cache = popup_audio_cache.clone();
    let note_subtitles = sub_ctrl.clone();
    let note_mpv = mpv.clone();
    let note_database_path = dictionary_database_path.clone();
    let note_resources_path = dictionary_resources_path.clone();
    let note_config_dir = config_dir.clone();
    let note_lookup_epoch = lookup_epoch.clone();
    app.on_add_anki_note(move |index| {
        let Some(app) = note_app.upgrade() else { return };
        if index < 0 { return }
        app.set_anki_added_note_id("".into());
        app.set_anki_added_result_index(-1);
        let definitions = app.get_dict_results();
        let Some(hit) = definitions.row_data(index as usize) else { return };
        let profile = if hit.is_kanji {
            note_config.anki_kanji_note_config()
        } else {
            note_config.anki_profile_note_config()
        };
        let Some(profile) = profile else {
            app.set_status_message("Select an Anki profile and configure this note type".into());
            return;
        };
        if !profile.enabled {
            app.set_status_message("Enable Anki integration in Preferences".into());
            return;
        }
        let mut context = controller::anki::NoteFieldsContext::new(
            hit.expression.to_string(), hit.reading.to_string(),
            app.get_active_sub_text().to_string(),
        );
        context.is_kanji = hit.is_kanji;
        let selected_glossary = app.global::<PopupBridge>().get_selected_glossary_text();
        if !selected_glossary.is_empty() {
            context.selection = selected_glossary.to_string();
        }
        // Never assign the scanned subtitle token into `context.selection` (#262);
        // use it purely to locate `cloze_prefix` / `cloze_body` / `cloze_suffix`.
        let start = app.get_selected_token_index();
        let (scanned_surface, token_offset_in_sentence) = if start >= 0 {
            let words = app.get_active_sub_words();
            let start_idx = start as usize;
            let count = app.get_selected_token_count().max(1) as usize;
            let byte_offset: usize = (0..start_idx)
                .filter_map(|i| words.row_data(i))
                .map(|word| word.text.len())
                .sum();
            let surface: String = (start_idx..start_idx.saturating_add(count))
                .filter_map(|i| words.row_data(i))
                .map(|word| word.text.to_string())
                .collect();
            (surface, Some(byte_offset))
        } else {
            (String::new(), None)
        };
        let (remove_regex, replace_lines, replace_with) = (
            app.get_search_remove_regex().to_string(),
            app.get_search_replace_lines(),
            app.get_search_replace_with().to_string(),
        );
        let (active_start, active_end, context_start, context_end, selected_cues, active_pos_in_selected) = {
            let subtitles = note_subtitles.borrow();
            let selected_indices = subtitles.selected_indices();
            let active_indices = subtitles.active_indices(subtitles.primary_sid());
            let active_pos = selected_indices.iter().position(|idx| active_indices.contains(idx));
            let cues = subtitles.selected_cues()
                .iter()
                .map(|cue| {
                    let raw = String::from_utf16_lossy(&cue.text);
                    filter_subtitle_text(&raw, &remove_regex, replace_lines, &replace_with)
                })
                .collect::<Vec<_>>();
            let active = subtitles.active_cue();
            let start = active.map(|cue| cue.start).unwrap_or(0.0);
            let end = active.map(|cue| cue.end).unwrap_or(0.0);
            let (ctx_start, ctx_end) = subtitles.selected_time_range().unwrap_or((start, end));
            (start, end, ctx_start, ctx_end, cues, active_pos)
        };
        controller::anki::apply_subtitle_cloze(
            &mut context,
            &selected_cues,
            active_pos_in_selected,
            &scanned_surface,
            token_offset_in_sentence,
        );
        context.secondary_subtitle = app.get_secondary_sub_text().to_string();
        context.title = app.get_media_title().to_string();
        context.dictionary_name = hit.dict_name.to_string();
        let mut occ_by_dict: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        let mut selected = Vec::new();
        let mut selected_occurrences = Vec::new();
        for row in (0..definitions.row_count()).filter_map(|r| definitions.row_data(r)) {
            if row.expression == hit.expression && row.reading == hit.reading
                && row.is_kanji == hit.is_kanji
            {
                let dict_name = row.dict_name.to_string();
                let occ = *occ_by_dict.entry(dict_name.clone()).and_modify(|c| *c += 1).or_insert(0);
                if row.selected {
                    selected_occurrences.push((dict_name, occ));
                    selected.push(row);
                }
            }
        }
        if selected.is_empty() {
            app.set_status_message("Select at least one dictionary entry for this term".into());
            return;
        }
        context.glossary_definitions = selected.iter().map(|row| controller::anki::GlossaryDefinition {
            dictionary_id: 0,
            dictionary_name: row.dict_name.to_string(),
            glossary: row.glossary.to_string(),
            tags: row.pos_tags.split(|character: char| character == ',' || character.is_whitespace())
                .filter(|tag| !tag.is_empty()).map(str::to_owned).collect(),
            notes: row.notes.to_string(),
            example_ja: row.example_ja.to_string(),
            example_en: row.example_en.to_string(),
            ..Default::default()
        }).collect();
        context.glossary_items = selected.iter().map(|row| row.glossary.to_string()).collect();
        context.tags = selected.iter().flat_map(|row| row.pos_tags.split(|character: char| {
            character == ',' || character.is_whitespace()
        })).filter(|tag| !tag.is_empty()).map(str::to_owned).collect();
        context.tags.sort();
        context.tags.dedup();
        if hit.is_kanji {
            context.character = hit.expression.to_string();
            for component in hit.reading.split(';') {
                let component = component.trim();
                if let Some(on) = component.strip_prefix("On:") { context.onyomi = on.trim().to_owned(); }
                if let Some(kun) = component.strip_prefix("Kun:") { context.kunyomi = kun.trim().to_owned(); }
            }
            context.stroke_count = hit.notes.split(';').filter_map(|part| {
                let (label, value) = part.split_once(':')?;
                (label.to_ascii_lowercase().contains("stroke") || label.contains("画数"))
                    .then(|| value.trim().to_owned())
            }).next().unwrap_or_default();
        }
        let (wants_screenshot, wants_video_screenshot) =
            controller::anki_media::requested_screenshots(&profile);
        let mut screenshot_request = None;
        let mut video_screenshot_request = None;
        let mut media_input = {
            let player_ref = note_mpv.borrow();
            let player = player_ref.as_ref();
            let media = player.and_then(MpvInstance::capture_encode_context)
                .map(|ctx| controller::media::MediaContext::new(ctx.path, ctx.audio_id));
            let mpv_start = player.map(MpvInstance::get_sub_start).unwrap_or(0.0);
            let mpv_end = player.map(MpvInstance::get_sub_end).unwrap_or(0.0);
            let subtitle_start = if active_end > active_start { active_start } else { mpv_start };
            let subtitle_end = if active_end > active_start { active_end } else { mpv_end };
            for (wanted, subtitles, request) in [
                (wants_screenshot, true, &mut screenshot_request),
                (wants_video_screenshot, false, &mut video_screenshot_request),
            ] {
                if wanted {
                    *request = Some(player.ok_or_else(|| "No video is loaded".to_owned())
                        .and_then(|player| player.request_screenshot(subtitles)));
                }
            }
            controller::anki_media::NoteMediaInput {
                media,
                subtitle_start,
                subtitle_end,
                context_start: if context_end > context_start { context_start } else { subtitle_start },
                context_end: if context_end > context_start { context_end } else { subtitle_end },
                screenshot: None,
                screenshot_video: None,
                subtitle_id: player.map(MpvInstance::primary_sid).unwrap_or(0),
                audio_sources: {
                    let state = note_audio_cache.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                    let matching = state.expression == hit.expression.as_str() && state.reading == hit.reading.as_str();
                    let mut sources = if matching { state.sources.clone() } else { Vec::new() };
                    drop(state);
                    if sources.is_empty() { sources = note_config.audio_source_entries(); }
                    let selected = if matching { app.get_popup_selected_audio_source() } else { -1 };
                    if selected >= 0 {
                        sources = sources.into_iter().nth(selected as usize).into_iter().collect();
                    }
                    sources
                },
            }
        };
        context.secondary_context = controller::anki::secondary_context_for_range(
            note_subtitles.borrow().secondary_entries(),
            media_input.context_start,
            media_input.context_end,
            &context.secondary_subtitle,
        );
        let unavailable = controller::anki::unavailable_profile_markers(&profile);
        let show_tag = note_config.anki_show_tag();
        app.set_status_message("Adding note to Anki…".into());
        let response_app = note_app.clone();
        let response_epoch = note_lookup_epoch.clone();
        let request_epoch = response_epoch.load(Ordering::Acquire);
        let metadata_path = note_database_path.clone();
        let res_dir = note_resources_path.clone();
        let cfg_dir = note_config_dir.clone();
        std::thread::spawn(move || {
            media_input.screenshot = screenshot_request
                .and_then(|request| request.ok())
                .and_then(|request| request.wait().ok());
            media_input.screenshot_video = video_screenshot_request
                .and_then(|request| request.ok())
                .and_then(|request| request.wait().ok());
            let controller = AnkiController::new(profile.endpoint());
            let resource_roots = vec![cfg_dir.join("dict_resources"), res_dir];
            let _ = controller::anki::populate_note_context_from_sqlite(
                &mut context,
                &metadata_path,
                &resource_roots,
                &selected_occurrences,
            );
            let result = controller::anki_media::prepare_note_media(&profile, &mut context, media_input)
                .and_then(|()| controller.add_note_from_profile(&profile, &context));
            let added_profile = profile.clone();
            let expire_app = response_app.clone();
            let _ = response_app.upgrade_in_event_loop(move |app| {
                if app.get_dict_open() && response_epoch.load(Ordering::Acquire) == request_epoch {
                    if let Ok(note_id) = &result {
                        ui::mark_popup_term_anki_added(
                            &app,
                            index as usize,
                            *note_id,
                            &added_profile,
                            show_tag,
                        );
                        let note_id = note_id.to_string();
                        app.set_anki_added_note_id(note_id.clone().into());
                        app.set_anki_added_result_index(index);
                        slint::Timer::single_shot(std::time::Duration::from_secs(15), move || {
                            if let Some(app) = expire_app.upgrade() {
                                if app.get_anki_added_note_id() == note_id {
                                    app.set_anki_added_note_id("".into());
                                    app.set_anki_added_result_index(-1);
                                }
                            }
                        });
                    }
                }
                app.set_status_message(match result {
                    Ok(note_id) if unavailable.is_empty() => format!("Added Anki note {note_id}").into(),
                    Ok(note_id) => {
                        eprintln!("Anki note {note_id}: unavailable profile markers left empty: {}", unavailable.join(", "));
                        format!("Added Anki note {note_id}; some fields are empty").into()
                    },
                    Err(error) => {
                        eprintln!("Could not add Anki note: {error}");
                        format!("Could not add Anki note: {error}").into()
                    },
                });
            });
        });
    });
    let browse_app = app.as_weak();
    let browse_config = config_ctrl.clone();
    app.on_browse_anki_note(move |id| {
        let Some(query) = controller::anki::normalize_anki_browse_target(id.as_str()) else {
            return;
        };
        let endpoint = browse_config.anki_endpoint();
        let weak = browse_app.clone();
        std::thread::spawn(move || {
            let result = AnkiController::new(endpoint).client().open_browse(&query);
            if let Err(error) = result {
                let _ = weak.upgrade_in_event_loop(move |app| {
                    app.set_status_message(format!("Could not open Anki browser: {error}").into());
                });
            }
        });
    });
    let (furigana_sender, furigana_receiver) = std::sync::mpsc::channel::<(u64, String)>();
    let furigana_epoch = Arc::new(AtomicU64::new(0));
    let furigana_worker_app = app.as_weak();
    let furigana_worker_epoch = furigana_epoch.clone();
    std::thread::spawn(move || {
        let mut warned = false;
        while let Ok(mut request) = furigana_receiver.recv() {
            while let Ok(newer) = furigana_receiver.try_recv() { request = newer; }
            if request.0 != furigana_worker_epoch.load(Ordering::Acquire) { continue; }
            let result = controller::subtitles::subtitle_furigana_parts(&request.1);
            let report_error = result.is_err() && !warned;
            warned |= report_error;
            let epoch = furigana_worker_epoch.clone();
            let _ = furigana_worker_app.upgrade_in_event_loop(move |app| {
                if request.0 != epoch.load(Ordering::Acquire)
                    || !app.get_furigana_visible()
                    || app.get_active_sub_text().as_str() != request.1 { return; }
                match result {
                    Ok(parts) => {
                        ui::apply_subtitle_furigana(&app, &request.1, parts);
                    }
                    Err(error) => {
                        app.set_subtitle_furigana_pending(false);
                        if report_error { app.set_status_message(format!("Furigana unavailable: {error}").into()); }
                    }
                }
            });
        }
    });
    // ── Async External Subtitle Parser Worker ───────────────────────────────
    let (ext_sub_sender, ext_sub_receiver) =
        std::sync::mpsc::channel::<(u64, ExternalSubtitleLoad)>();
    let ext_sub_app = app.as_weak();
    let ext_sub_shared_gen = shared_media_generation.clone();
    let ext_sub_ui_gen = media_generation.clone();
    let ext_sub_ctrl = sub_ctrl.clone();
    let ext_sub_model = sub_model.clone();
    let ext_sub_coord = coordinator.clone();
    // Move Send-only clones into the parser worker; UI Rc handles stay inside upgrade_in_event_loop?
    // Wait: upgrade_in_event_loop requires a `Send` closure! Use a result channel or thread_local UI sink!
    let (ext_parsed_tx, ext_parsed_rx) = std::sync::mpsc::channel::<(
        u64,
        i64,
        String,
        Result<Vec<memento_subtitle::SubtitleEntry>, String>,
    )>();
    std::thread::spawn(move || {
        while let Ok((generation, load)) = ext_sub_receiver.recv() {
            if ext_sub_shared_gen.load(Ordering::Acquire) != generation {
                continue;
            }
            let parsed = parse_external_subtitles(&load.path);
            if ext_sub_shared_gen.load(Ordering::Acquire) != generation {
                continue;
            }
            let path_str = load.path.to_string_lossy().into_owned();
            if ext_parsed_tx
                .send((generation, load.track_id, path_str, parsed))
                .is_ok()
            {
                let _ = slint::invoke_from_event_loop(|| {
                    EXTERNAL_SUBTITLE_SINK.with(|slot| {
                        let cb = slot.borrow().clone();
                        if let Some(apply) = cb {
                            apply();
                        }
                    });
                });
            }
        }
    });

    thread_local! {
        static EXTERNAL_SUBTITLE_SINK: RefCell<Option<Rc<dyn Fn()>>> = const { RefCell::new(None) };
        static MPV_EVENT_SYNC: RefCell<Option<Rc<dyn Fn()>>> = const { RefCell::new(None) };
    }

    EXTERNAL_SUBTITLE_SINK.with(|slot| {
        let app_weak = ext_sub_app.clone();
        let ui_gen = ext_sub_ui_gen.clone();
        let subs_rc = ext_sub_ctrl.clone();
        let model_rc = ext_sub_model.clone();
        let sec_model_rc = secondary_sub_model.clone();
        let coord_rc = ext_sub_coord.clone();
        *slot.borrow_mut() = Some(Rc::new(move || {
            while let Ok((generation, track_id, path_str, result)) = ext_parsed_rx.try_recv() {
                let Some(app) = app_weak.upgrade() else {
                    continue;
                };
                if ui_gen.get() != generation {
                    continue;
                }
                let Ok(mut subtitles) = subs_rc.try_borrow_mut() else {
                    continue;
                };
                match result {
                    Ok(cues) => {
                        if app.get_furigana_visible() {
                            let lines: Vec<String> = cues
                                .iter()
                                .take(2048)
                                .map(|cue| String::from_utf16_lossy(&cue.text))
                                .collect();
                            controller::subtitles::prewarm_subtitle_furigana(lines);
                        }
                        subtitles.set_external_cues(track_id, &path_str, cues);
                    }
                    Err(error) => {
                        subtitles.set_external_cues(track_id, &path_str, Vec::new());
                        app.set_status_message(error.into());
                    }
                }
                if let Ok(coord_guard) = coord_rc.try_borrow() {
                    if let Some(coord) = coord_guard.as_ref() {
                        let st = coord.playback_state();
                        let speed = st.sub_speed.max(f64::EPSILON);
                        let raw_primary = (st.position - st.sub_delay) / speed;
                        let raw_secondary = (st.position - st.secondary_sub_delay) / speed;
                        subtitles.update_positions(raw_primary, raw_secondary);
                    }
                }
                refresh_subtitle_list(&app, &model_rc, &subtitles);
                refresh_secondary_subtitle_list(&app, &sec_model_rc, &subtitles);
            }
        }));
    });

    // ── Event-Driven Playback Synchronization (`set_wakeup_callback` + Low-Freq Tick) ──
    let run_player_sync: Rc<dyn Fn()> = {
        let sync_app = app.as_weak();
        let sync_mpv = mpv.clone();
        let sync_coord = coordinator.clone();
        let sync_sub_ctrl = sub_ctrl.clone();
        let sync_sub_model = sub_model.clone();
        let sync_sec_sub_model = secondary_sub_model.clone();
        let sync_media_generation = media_generation.clone();
        let sync_shared_gen = shared_media_generation.clone();
        let sync_current_media = current_media.clone();
        let sync_lookup_epoch = lookup_epoch.clone();
        let sync_asr = asr_session.clone();
        let sync_asr_config = config_ctrl.clone();
        let sync_asr_restart = asr_restart_pending.clone();
        let sync_closing = closing.clone();
        let sync_hover_owned = hover_pause_owned.clone();
        let sync_furigana_sender = furigana_sender.clone();
        let sync_furigana_epoch = furigana_epoch.clone();
        let sync_video_timings = video_timings.clone();
        let sync_pending_frame = pending_playback_frame.clone();
        let sync_ext_sub_tx = ext_sub_sender.clone();
        let sync_screensaver = screensaver.clone();
        let verbose_video_timings = std::env::var_os("MEMENTO_VIDEO_STATS").is_some();
        let previous_furigana_visible = Cell::new(app.get_furigana_visible());
        let previous_subtitle_layout: Cell<Option<(i32, i32, u32, bool)>> = Cell::new(None);
        let previous_chapters: RefCell<Vec<f64>> = RefCell::new(Vec::new());
        let previous_chapters_dur = Cell::new(0.0f64);
        let previous_title_pair: RefCell<(String, String)> = RefCell::new((String::new(), String::new()));
        let previous_sec_state: Cell<(i64, usize, Option<usize>)> = Cell::new((0, 0, None));
        let previous_playback_active: Cell<Option<bool>> = Cell::new(None);
        let previous_active_scroll: Cell<(bool, i32, Option<usize>, Option<usize>)> =
            Cell::new((false, 0, None, None));
        let last_video_report = Cell::new(std::time::Instant::now());
        let previous_drops = Cell::new((0i64, 0i64));
        let last_sync = Cell::new(std::time::Instant::now());
        let last_autofit_generation: Cell<Option<u64>> = Cell::new(None);

        Rc::new(move || {
            if sync_closing.get() {
                return;
            }
            let sync_started = std::time::Instant::now();
            {
                let mut timings = sync_video_timings.borrow_mut();
                timings.sync_gap_max = timings
                    .sync_gap_max
                    .max(sync_started.duration_since(last_sync.get()));
            }
            last_sync.set(sync_started);
            let _sync_timing = SyncTiming {
                started: sync_started,
                timings: sync_video_timings.clone(),
            };
            let Some(app) = sync_app.upgrade() else {
                return;
            };
            let (Ok(player_ref), Ok(mut coord_ref), Ok(mut subtitles), Ok(mut asr)) = (
                sync_mpv.try_borrow(),
                sync_coord.try_borrow_mut(),
                sync_sub_ctrl.try_borrow_mut(),
                sync_asr.try_borrow_mut(),
            ) else {
                return;
            };
            let (Some(player), Some(coord)) = (player_ref.as_ref(), coord_ref.as_mut()) else {
                return;
            };

            let gen_before = sync_media_generation.get();
            let external_loads = coord.sync(
                &app,
                player,
                &mut subtitles,
                &sync_sub_model,
                &mut asr,
                &sync_media_generation,
                &sync_current_media,
                &sync_asr_restart,
                &sync_hover_owned,
                || {
                    let config_dir = memento_xdg_dir("XDG_CONFIG_HOME", ".config")
                        .unwrap_or_else(|| PathBuf::from("."));
                    controller::asr::AsrSettings::from_config(
                        &config_dir,
                        &*sync_asr_config.store(),
                    )
                },
                |app, primary_text| {
                    sync_lookup_epoch.fetch_add(1, Ordering::AcqRel);
                    publish_active_subtitle(
                        app,
                        primary_text,
                        &sync_furigana_sender,
                        &sync_furigana_epoch,
                    );
                },
            );
            let gen_after = sync_media_generation.get();
            if gen_after != gen_before {
                sync_shared_gen.store(gen_after, Ordering::Release);
            }
            for load in external_loads {
                let _ = sync_ext_sub_tx.send((gen_after, load));
            }

            let state = coord.playback_state();
            if state.file_loaded && last_autofit_generation.get() != Some(gen_after) {
                let (vw, vh) = if state.dimensions.0 > 0 && state.dimensions.1 > 0 {
                    state.dimensions
                } else if state.video_width > 0 && state.video_height > 0 {
                    (state.video_width as u32, state.video_height as u32)
                } else {
                    (0, 0)
                };
                if vw > 0 && vh > 0 {
                    last_autofit_generation.set(Some(gen_after));
                    let sf = app.window().scale_factor().max(0.5);
                    let cur_phys = app.window().size();
                    let max_w = ((cur_phys.width as f32) / sf).max(1920.0);
                    let max_h = ((cur_phys.height as f32) / sf).max(1080.0);
                    if let Some((fit_w, fit_h)) = compute_autofit_window_size(
                        vw,
                        vh,
                        app.get_autofit_enabled(),
                        app.get_autofit_percent(),
                        app.window().is_fullscreen() || app.get_is_fullscreen(),
                        app.window().is_maximized(),
                        app.get_dict_search_open() || app.get_sub_drawer_open(),
                        app.get_sidebar_width(),
                        (max_w, max_h),
                    ) {
                        app.window().set_size(slint::LogicalSize::new(fit_w, fit_h));
                    }
                }
            }
            if app.get_is_muted() != state.mute {
                app.set_is_muted(state.mute);
            }
            let effective_vol_max = (state.volume_max.max(100) as f32).clamp(100.0, 1000.0);
            if (app.get_volume_max() - effective_vol_max).abs() > 0.5 {
                app.set_volume_max(effective_vol_max);
            }
            let playback_active = state.file_loaded && !state.paused;
            if previous_playback_active.replace(Some(playback_active)) != Some(playback_active) {
                if let Ok(mut ss) = sync_screensaver.try_borrow_mut() {
                    ss.sync_playback(state.file_loaded, state.paused, "Playing media");
                }
            }
            let prim_active = subtitles
                .active_indices(subtitles.primary_sid())
                .first()
                .copied();
            let sec_sid = subtitles.secondary_sid();
            let sec_count = subtitles.secondary_entries().len();
            let sec_active = subtitles.active_indices(sec_sid).first().copied();
            if previous_sec_state.replace((sec_sid, sec_count, sec_active))
                != (sec_sid, sec_count, sec_active)
            {
                refresh_secondary_subtitle_list(&app, &sync_sec_sub_model, &subtitles);
            }
            let scroll_state = (
                app.get_sub_drawer_open(),
                app.get_subtitle_drawer_tab(),
                prim_active,
                sec_active,
            );
            if previous_active_scroll.replace(scroll_state) != scroll_state {
                auto_scroll_subtitle_drawer_if_needed(&app, &subtitles);
            }
            if !state.path.is_empty() && !state.media_title.is_empty() {
                let mut prev_pair = previous_title_pair.borrow_mut();
                if prev_pair.0 != state.path || prev_pair.1 != state.media_title {
                    *prev_pair = (state.path.clone(), state.media_title.clone());
                    drop(prev_pair);
                    record_recent_media_title(&state.path, &state.media_title);
                    refresh_recent_files(&app, &sync_asr_config);
                }
            }
            {
                let mut prev_ch = previous_chapters.borrow_mut();
                if *prev_ch != state.chapters
                    || (previous_chapters_dur.get() - state.duration).abs() > 0.5
                {
                    *prev_ch = state.chapters.clone();
                    previous_chapters_dur.set(state.duration);
                    let raw_ch: Vec<f32> = state.chapters.iter().map(|&t| t as f32).collect();
                    let marks = chapter_marks_for_duration(&state.chapters, state.duration);
                    app.set_chapters(Rc::new(VecModel::from(raw_ch)).into());
                    app.set_chapter_marks(Rc::new(VecModel::from(marks)).into());
                }
            }

            if app.get_pending_playback_generation() >= 0 && state.playback_ready {
                if player.is_opengl_active() {
                    sync_pending_frame.set(Some(app.get_pending_playback_generation()));
                    app.window().request_redraw();
                } else {
                    app.set_pending_playback_generation(-1);
                    let _ = player.play();
                }
            }
            if last_video_report.get().elapsed() >= std::time::Duration::from_secs(5) {
                let drops = (state.frame_drops, state.decoder_drops);
                sync_video_timings.borrow_mut().report(
                    state,
                    drops != previous_drops.get(),
                    verbose_video_timings,
                    player.pending_render_delay(),
                );
                previous_drops.set(drops);
                last_video_report.set(std::time::Instant::now());
            }

            if previous_furigana_visible.get() != app.get_furigana_visible() {
                previous_furigana_visible.set(app.get_furigana_visible());
                publish_active_subtitle(
                    &app,
                    &app.get_active_sub_text(),
                    &sync_furigana_sender,
                    &sync_furigana_epoch,
                );
            }
            let subtitle_layout = (
                app.get_video_width(),
                app.get_video_height(),
                app.get_subtitle_scale().to_bits(),
                app.get_furigana_visible(),
            );
            if previous_subtitle_layout.get() != Some(subtitle_layout) {
                previous_subtitle_layout.set(Some(subtitle_layout));
                let words: Vec<_> = app.get_active_sub_words().iter().collect();
                if !words.is_empty() {
                    let lines = subtitle_lines_for_app(&app, words);
                    app.set_active_sub_lines(Rc::new(VecModel::from(lines)).into());
                }
            }
        })
    };

    MPV_EVENT_SYNC.with(|slot| {
        *slot.borrow_mut() = Some(run_player_sync.clone());
    });
    if let Some(player) = mpv.borrow().as_ref() {
        player.set_wakeup_callback(|| {
            let _ = slint::invoke_from_event_loop(|| {
                let cb = MPV_EVENT_SYNC.with(|slot| slot.borrow().clone());
                if let Some(sync) = cb {
                    sync();
                }
            });
        });
    }

    // Lightweight 200ms fallback/housekeeping tick: expires transient status
    // messages and drains background ASR events or active playback without
    // redundant property dirtying when paused & idle.
    let timer = slint::Timer::default();
    let timer_app = app.as_weak();
    let timer_mpv = mpv.clone();
    let timer_asr = asr_session.clone();
    let timer_asr_restart = asr_restart_pending.clone();
    let timer_closing = closing.clone();
    let timer_sync = run_player_sync.clone();
    let mut status_message_ticks = 0usize;
    let mut last_observed_status = slint::SharedString::default();
    let mut last_ui_flags = (
        app.get_subtitles_visible(),
        app.get_hide_mpv_subtitles(),
        app.get_hide_memento_while_playing(),
        app.get_auto_pause(),
        app.get_furigana_visible(),
        app.get_video_width(),
        app.get_video_height(),
        app.get_subtitle_scale().to_bits(),
        app.get_search_remove_regex(),
        app.get_search_replace_lines(),
        app.get_search_replace_with(),
    );
    timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(200),
        move || {
            if timer_closing.get() {
                return;
            }
            let Some(app) = timer_app.upgrade() else {
                return;
            };
            let current_status = app.get_status_message();
            if !current_status.is_empty() {
                if current_status != last_observed_status {
                    last_observed_status = current_status;
                    status_message_ticks = 0;
                } else {
                    status_message_ticks += 1;
                    if status_message_ticks >= 20 {
                        app.set_status_message("".into());
                        last_observed_status = "".into();
                        status_message_ticks = 0;
                    }
                }
            } else {
                last_observed_status = "".into();
                status_message_ticks = 0;
            }

            let ui_flags = (
                app.get_subtitles_visible(),
                app.get_hide_mpv_subtitles(),
                app.get_hide_memento_while_playing(),
                app.get_auto_pause(),
                app.get_furigana_visible(),
                app.get_video_width(),
                app.get_video_height(),
                app.get_subtitle_scale().to_bits(),
                app.get_search_remove_regex(),
                app.get_search_replace_lines(),
                app.get_search_replace_with(),
            );
            let ui_flags_changed = ui_flags != last_ui_flags;
            last_ui_flags = ui_flags;

            let needs_sync = ui_flags_changed
                || timer_asr_restart.get()
                || timer_asr.try_borrow().is_ok_and(|asr| asr.is_active())
                || timer_mpv.try_borrow().is_ok_and(|p| {
                    p.as_ref().is_some_and(|player| {
                        player.has_pending_events() || !player.is_paused()
                    })
                });
            if needs_sync {
                timer_sync();
            }
        },
    );

    // The normal path renders libmpv directly into a texture owned by Slint's
    // OpenGL context. This avoids the CPU color conversion and per-frame RGBA
    // upload of mpv's software renderer. Keep software rendering as fallback
    // for a software-only Slint backend or an unavailable GL render context.
    let gl_surface = Rc::new(RefCell::new(None::<GlVideoSurface>));
    let gl_pending = Rc::new(Cell::new(true));
    let gl_target_size = Rc::new(Cell::new((0u32, 0u32)));
    let gl_ready_texture = Rc::new(Cell::new(None::<(std::num::NonZeroU32, u32, u32)>));
    let gl_mpv = mpv.clone();
    let notifier_surface = gl_surface.clone();
    let notifier_pending = gl_pending.clone();
    let notifier_size = gl_target_size.clone();
    let notifier_render_size = current_render_size.clone();
    let notifier_texture = gl_ready_texture.clone();
    let notifier_app = app.as_weak();
    let notifier_timings = video_timings.clone();
    let notifier_pending_frame = pending_playback_frame.clone();
    let notifier_trigger_initial = trigger_initial_loads.clone();
    if let Err(error) = app.window().set_rendering_notifier(move |state, graphics_api| {
        let slint::GraphicsAPI::NativeOpenGL { get_proc_address } = graphics_api else {
            if matches!(state, slint::RenderingState::RenderingSetup) {
                notifier_trigger_initial();
            }
            return;
        };
        match state {
            slint::RenderingState::RenderingSetup => {
                {
                    let player_ref = gl_mpv.borrow();
                    if let Some(player) = player_ref.as_ref() {
                        let (x11_display, wl_display) = notifier_app
                            .upgrade()
                            .map(|app| native_window_display_pointers(app.window()))
                            .unwrap_or((None, None));
                        #[cfg(target_os = "linux")]
                        let gl_res = if let Some(owned_resolver) =
                            build_owned_gl_proc_resolver(*get_proc_address, wl_display.is_some())
                        {
                            player.activate_opengl_owned_with_display(
                                owned_resolver,
                                x11_display,
                                wl_display,
                            )
                        } else {
                            player.activate_opengl_with_display(
                                *get_proc_address,
                                x11_display,
                                wl_display,
                            )
                        };
                        #[cfg(not(target_os = "linux"))]
                        let gl_res = player.activate_opengl_with_display(
                            *get_proc_address,
                            x11_display,
                            wl_display,
                        );
                        match gl_res {
                            Ok(()) => {
                                *notifier_surface.borrow_mut() =
                                    Some(GlVideoSurface::new(*get_proc_address, player));
                                notifier_pending.set(true);
                                eprintln!("Video renderer: libmpv OpenGL");
                            }
                            Err(error) => eprintln!(
                                "OpenGL video unavailable; using software rendering: {error}"
                            ),
                        }
                    }
                }
                notifier_trigger_initial();
            }
            slint::RenderingState::BeforeRendering => {
                {
                    let mut timings = notifier_timings.borrow_mut();
                    timings.paint_started = Some(std::time::Instant::now());
                    timings.paint_popup_rows = notifier_app
                        .upgrade()
                        .filter(|app| app.get_dict_open())
                        .map_or(0, |app| app.get_popup_results().row_count());
                }
                let size = notifier_size.get();
                if size == (0, 0) {
                    return;
                }
                let player_ref = gl_mpv.borrow();
                let Some(player) = player_ref.as_ref() else {
                    return;
                };
                let mut surface_ref = notifier_surface.borrow_mut();
                let Some(surface) = surface_ref.as_mut() else {
                    return;
                };
                // Check `size == (0, 0)` BEFORE `surface.update(player)` (`take_render_update()`)
                // so early frames before initial viewport layout are never dropped!
                let frame_ready = surface.update(player);
                let needs_realloc = surface.should_reallocate(size);
                if !notifier_pending.replace(false) && !frame_ready && !needs_realloc {
                    return;
                }
                let render_started = std::time::Instant::now();
                if let Err(error) = surface.ensure_size(size).and_then(|()| surface.render(player))
                {
                    eprintln!("OpenGL video failed; switching to software rendering: {error}");
                    notifier_texture.set(None);
                    notifier_render_size.set((0, 0));
                    if let Some(old_surface) = surface_ref.take() {
                        old_surface.destroy();
                    }
                    if let Err(fallback_error) = player.fallback_to_software() {
                        eprintln!("Software video fallback failed: {fallback_error}");
                    }
                    let _ = notifier_app.upgrade_in_event_loop(|app| app.invoke_update_video());
                } else {
                    notifier_render_size.set(surface.size);
                    let texture = surface.texture_info();
                    if notifier_texture.replace(texture) != texture {
                        // Publishing an Image inside the rendering notifier
                        // re-enters Slint's window RefCell. Do it after rendering.
                        let _ = notifier_app.upgrade_in_event_loop(|app| app.invoke_update_video());
                    }
                }
                let mut timings = notifier_timings.borrow_mut();
                timings.frames += 1;
                timings.video_max = timings.video_max.max(render_started.elapsed());
                if frame_ready {
                    timings.queue_max = timings.queue_max.max(player.render_update_delay());
                }
            }
            slint::RenderingState::AfterRendering => {
                if notifier_surface.borrow().is_some() {
                    if let Some(player) = gl_mpv.borrow().as_ref() {
                        player.report_render_swap();
                    }
                }
                notifier_timings.borrow_mut().finish_paint();
                let presented_at = std::time::Instant::now();
                let timings = notifier_timings.clone();
                // Slint presents after AfterRendering returns. Measure the
                // time until control returns to the event loop as well.
                slint::Timer::single_shot(std::time::Duration::ZERO, move || {
                    let mut timings = timings.borrow_mut();
                    timings.present_max = timings.present_max.max(presented_at.elapsed());
                });
                if notifier_texture.get().is_some() {
                    if let Some(generation) = notifier_pending_frame.take() {
                        // This runs after Slint returns from drawing/presenting,
                        // never while its rendering/window RefCells are borrowed.
                        let _ = notifier_app.upgrade_in_event_loop(move |app| {
                            app.invoke_video_load_presented(generation);
                        });
                    }
                }
            }
            slint::RenderingState::RenderingTeardown => {
                notifier_texture.set(None);
                notifier_render_size.set((0, 0));
                if let Some(player) = gl_mpv.borrow().as_ref() {
                    player.deactivate_opengl();
                }
                if let Some(surface) = notifier_surface.borrow_mut().take() {
                    surface.destroy();
                }
            }
            _ => {}
        }
    }) {
        eprintln!("OpenGL video integration unavailable; using software rendering: {error}");
        trigger_initial_loads();
    }

    // Fallback CPU renderer runs off the UI thread and is inactive while GL
    // rendering is available. Bounded channels discard stale frames.
    let (render_request_tx, render_request_rx) = std::sync::mpsc::sync_channel::<(u32, u32)>(1);
    let (render_frame_tx, render_frame_rx) =
        std::sync::mpsc::sync_channel::<SharedPixelBuffer<Rgba8Pixel>>(1);
    let software_frame_app = app.as_weak();
    let renderer = mpv.borrow().as_ref().map(MpvInstance::software_renderer);
    let render_thread = renderer.map(move |renderer| std::thread::spawn(move || {
        let mut frame_pool: [Option<SharedPixelBuffer<Rgba8Pixel>>; 3] = [None, None, None];
        let mut pool_idx = 0usize;
        while let Ok(mut size) = render_request_rx.recv() {
            if size == (0, 0) { break; }
            while let Ok(newer) = render_request_rx.try_recv() { size = newer; }
            if size == (0, 0) { break; }
            let mut buffer = match frame_pool[pool_idx].take() {
                Some(buf) if buf.width() == size.0 && buf.height() == size.1 => buf,
                _ => SharedPixelBuffer::<Rgba8Pixel>::new(size.0, size.1),
            };
            // VideoRenderWorker joins before the mpv instance can be dropped.
            if let Err(error) = unsafe { renderer.render_rgba(size.0, size.1, buffer.make_mut_bytes()) } {
                eprintln!("Cannot render video frame: {error}");
                frame_pool[pool_idx] = Some(buffer);
                continue;
            }
            if render_frame_tx.try_send(buffer.clone()).is_ok() {
                let _ = software_frame_app.upgrade_in_event_loop(|app| app.invoke_update_video());
            }
            frame_pool[pool_idx] = Some(buffer);
            pool_idx = (pool_idx + 1) % frame_pool.len();
        }
    }));
    let render_worker = VideoRenderWorker {
        stop: render_request_tx.clone(),
        thread: render_thread,
    };
    let video_app = app.as_weak();
    let video_mpv = mpv.clone();
    let video_requests = render_request_tx.clone();
    let video_gl_pending = gl_pending.clone();
    let video_gl_target_size = gl_target_size.clone();
    let video_gl_ready_texture = gl_ready_texture.clone();
    let video_current_render_size = current_render_size.clone();
    let mut published_gl_texture = None;
    let mut rendered_size = (0, 0);
    app.on_update_video(move || {
            let Some(app) = video_app.upgrade() else { return };
            if app.get_video_width() <= 0 || app.get_video_height() <= 0 {
                video_gl_target_size.set((0, 0));
                video_current_render_size.set((0, 0));
                if published_gl_texture.take().is_some() {
                    app.set_video_frame(slint::Image::default());
                }
                return;
            }
            let Ok(player_ref) = video_mpv.try_borrow() else { return };
            let Some(player) = player_ref.as_ref() else { return };
            if player.is_opengl_active() {
                rendered_size = (0, 0);
                while render_frame_rx.try_recv().is_ok() {}
                if let Some(texture) = video_gl_ready_texture.get() {
                    if published_gl_texture != Some(texture) {
                        app.set_video_frame(unsafe {
                            slint::BorrowedOpenGLTextureBuilder::new_gl_2d_rgba_texture(
                                texture.0, [texture.1, texture.2].into(),
                            ).origin(slint::BorrowedOpenGLTextureOrigin::TopLeft).build()
                        });
                        published_gl_texture = Some(texture);
                    }
                }
                let size = video_render_size(&app, player, true);
                if video_gl_target_size.replace(size) != size {
                    video_gl_pending.set(true);
                    app.window().request_redraw();
                }
                if player.has_render_update() {
                    app.window().request_redraw();
                }
                return;
            }
            if published_gl_texture.take().is_some() {
                app.set_video_frame(slint::Image::default());
            }
            let mut latest_frame = None;
            while let Ok(frame) = render_frame_rx.try_recv() { latest_frame = Some(frame); }
            if let Some(frame) = latest_frame { app.set_video_frame(slint::Image::from_rgba8(frame)); }
            let (mut target_w, mut target_h) = video_render_size(&app, player, false);
            // mpv's software API converts and scales on one CPU thread. Cap
            // fallback output area so a 1080p window does not upload 8 MB on
            // every frame if GL cannot be used.
            let area = u64::from(target_w) * u64::from(target_h);
            if area > 1280 * 720 {
                let ratio = ((1280 * 720) as f64 / area as f64).sqrt();
                target_w = ((target_w as f64 * ratio).round() as u32).max(1);
                target_h = ((target_h as f64 * ratio).round() as u32).max(1);
            }
            video_current_render_size.set((target_w, target_h));

            if rendered_size == (target_w, target_h) && !player.take_render_update() {
                return;
            }
            if video_requests.try_send((target_w, target_h)).is_ok() {
                rendered_size = (target_w, target_h);
            }
    });

    let ready_app = app.as_weak();
    let ready_mpv = mpv.clone();
    let ready_timings = video_timings.clone();
    app.on_video_frame_ready(move || {
        let Some(app) = ready_app.upgrade() else { return };
        let Ok(player_ref) = ready_mpv.try_borrow() else { return };
        let Some(player) = player_ref.as_ref() else { return };
        let mut timings = ready_timings.borrow_mut();
        timings.wake_max = timings.wake_max.max(player.pending_render_delay());
        drop(timings);
        if player.is_opengl_active() {
            // A frame changes the shared texture, not the scene geometry.
            // Don't evaluate video/subtitle preferred sizes on every wakeup.
            app.window().request_redraw();
        } else {
            app.invoke_update_video();
        }
    });

    // mpv wakes Slint as soon as a frame is ready. Coalescing happens in the
    // player callback; there is no frame-rate polling timer in the GL path.
    let frame_ready_app = app.as_weak();
    if let Some(player) = mpv.borrow().as_ref() {
        player.set_render_wakeup(move || {
            let _ = frame_ready_app.upgrade_in_event_loop(|app| app.invoke_video_frame_ready());
        });
    }
    app.invoke_update_video();

    match determine_startup_update_action(&config_ctrl) {
        StartupUpdateAction::ShowOptInPrompt => open_update_opt_in_dialog(&app),
        StartupUpdateAction::RunSilentCheck => spawn_update_check(app.as_weak(), true),
        StartupUpdateAction::Nothing => {}
    }

    let run_result = app.run();
    timer.stop();
    quit_timer.stop();
    screensaver.borrow_mut().uninhibit();
    MPV_EVENT_SYNC.with(|slot| *slot.borrow_mut() = None);
    EXTERNAL_SUBTITLE_SINK.with(|slot| *slot.borrow_mut() = None);
    OPEN_PATHS_SINK.with(|slot| *slot.borrow_mut() = None);
    drop(ext_sub_sender);
    drop(thumb_tx);
    // Also cover an event-loop error or any exit that bypasses the close callback.
    asr_session.borrow_mut().shutdown();
    drop(render_worker);
    drop(render_request_tx);
    run_result?;
    Ok(())
}

#[cfg(test)]
mod xdg_migration_tests {
    use super::migrate_xdg_config_if_needed;

    #[test]
    fn migrates_legacy_cache_config_only_when_config_missing() {
        let root = tempfile::TempDir::new().unwrap();
        let legacy_dir = root.path().join("cache/memento");
        let config_dir = root.path().join("config/memento");
        std::fs::create_dir_all(&legacy_dir).unwrap();
        std::fs::write(legacy_dir.join("memento.conf"), "legacy=1\n").unwrap();

        assert!(migrate_xdg_config_if_needed(&legacy_dir, &config_dir, "memento.conf").unwrap());
        assert_eq!(
            std::fs::read_to_string(config_dir.join("memento.conf")).unwrap(),
            "legacy=1\n"
        );

        std::fs::write(config_dir.join("memento.conf"), "existing=2\n").unwrap();
        assert!(!migrate_xdg_config_if_needed(&legacy_dir, &config_dir, "memento.conf").unwrap());
        assert_eq!(
            std::fs::read_to_string(config_dir.join("memento.conf")).unwrap(),
            "existing=2\n"
        );
    }
}

#[cfg(test)]
mod window_ux_tests {
    use super::*;
    use slint::platform::{Key, PointerEventButton, WindowEvent};

    #[test]
    fn slint_pua_keys_and_bare_modifiers_translate_cleanly_in_keyboard_chord() {
        assert_eq!(keyboard_chord("\u{F700}", true, false, false, false), "Ctrl+Up");
        assert_eq!(keyboard_chord("\u{F701}", true, false, false, false), "Ctrl+Down");
        assert_eq!(keyboard_chord("\u{F702}", false, false, true, false), "Alt+Left");
        assert_eq!(keyboard_chord("\u{F703}", false, false, true, false), "Alt+Right");
        assert_eq!(keyboard_chord("\u{F704}", false, false, false, false), "F1");
        assert_eq!(keyboard_chord("\u{F706}", false, false, false, false), "F3");
        assert_eq!(keyboard_chord("\u{F706}", false, true, false, false), "Shift+F3");
        assert_eq!(keyboard_chord("\u{F70F}", false, false, false, false), "F12");
        assert_eq!(keyboard_chord("\u{007f}", false, false, false, false), "Delete");
        assert_eq!(keyboard_chord("\u{F728}", false, false, false, false), "Delete");
        assert_eq!(keyboard_chord("\u{001b}", false, false, false, false), "Escape");
        assert_eq!(keyboard_chord("\u{0008}", false, false, false, false), "Backspace");
        assert_eq!(keyboard_chord("\u{0009}", false, false, false, false), "Tab");
        assert_eq!(keyboard_chord("\u{000a}", false, false, false, false), "Return");
        assert_eq!(keyboard_chord("c", true, true, false, false), "Ctrl+Shift+C");
        assert_eq!(keyboard_chord("d", true, false, false, false), "Ctrl+D");

        for bare_mod in [
            "",
            "\u{0010}",
            "\u{0011}",
            "\u{0012}",
            "\u{0013}",
            "\u{0014}",
            "\u{0015}",
            "\u{0016}",
            "\u{0017}",
            "\u{0018}",
            "Shift",
            "Control",
            "Ctrl",
            "Alt",
            "Meta",
            "Super",
        ] {
            assert!(is_bare_modifier_key(bare_mod));
            assert_eq!(keyboard_chord(bare_mod, true, true, true, true), "");
        }
        assert_eq!(bare_modifier_flags(&String::from(char::from(Key::Shift))), (false, true, false, false));
        assert_eq!(bare_modifier_flags(&String::from(char::from(Key::ShiftR))), (false, true, false, false));
        assert_eq!(bare_modifier_flags(&String::from(char::from(Key::Control))), (true, false, false, false));
        assert_eq!(bare_modifier_flags(&String::from(char::from(Key::ControlR))), (true, false, false, false));
        assert_eq!(bare_modifier_flags(&String::from(char::from(Key::Alt))), (false, false, true, false));
        assert_eq!(bare_modifier_flags(&String::from(char::from(Key::AltGr))), (false, false, true, false));
        assert_eq!(bare_modifier_flags(&String::from(char::from(Key::Meta))), (false, false, false, true));
        assert_eq!(bare_modifier_flags(&String::from(char::from(Key::MetaR))), (false, false, false, true));
        assert_eq!(bare_modifier_flags(&String::from(char::from(Key::CapsLock))), (false, false, false, false));
        assert_eq!(bare_modifier_flags("Shift"), (false, true, false, false));
        assert_eq!(bare_modifier_flags("ShiftR"), (false, true, false, false));
        assert_eq!(bare_modifier_flags("Control"), (true, false, false, false));
        assert_eq!(bare_modifier_flags("Ctrl"), (true, false, false, false));
        assert_eq!(bare_modifier_flags("Alt"), (false, false, true, false));
        assert_eq!(bare_modifier_flags("Meta"), (false, false, false, true));
        assert_eq!(bare_modifier_flags("Super"), (false, false, false, true));
    }

    #[test]
    fn bare_modifiers_and_popup_shortcuts_preserve_yomitan_popup_and_all_eight_keybinds_work() {
        i_slint_backend_testing::init_no_event_loop();
        let app = MementoApp::new().unwrap();
        app.window().set_size(slint::LogicalSize::new(1280., 720.));
        let subtitles = Rc::new(RefCell::new(SubtitleController::new()));
        subtitles.borrow_mut().load_cues(vec![
            controller::subtitles::create_cue("First match line", 1., 2.),
            controller::subtitles::create_cue("Second middle line", 3., 4.),
            controller::subtitles::create_cue("Third match line", 5., 6.),
        ]);
        let rows = Rc::new(VecModel::<SubEntry>::default());
        app.set_subtitle_entries(rows.clone().into());
        refresh_subtitle_list(&app, &rows, &subtitles.borrow());
        install_subtitle_selection(&app, subtitles.clone(), rows.clone());
        app.show().unwrap();

        // Open YomitanPopup with two results and verify bare modifiers never dismiss it.
        set_dictionary_rows(
            &app,
            vec![
                DictResult {
                    index: 0,
                    expression: "一話".into(),
                    reading: "いちわ".into(),
                    glossary: "episode one".into(),
                    dict_name: "JMdict".into(),
                    selected: true,
                    term_start: true,
                    term_end: true,
                    ..Default::default()
                },
                DictResult {
                    index: 1,
                    expression: "旅立ち".into(),
                    reading: "たびだち".into(),
                    glossary: "setting off on a journey".into(),
                    dict_name: "JMdict".into(),
                    selected: true,
                    term_start: true,
                    term_end: true,
                    ..Default::default()
                },
            ],
        );
        app.set_dict_open(true);
        for key in [Key::Shift, Key::Control, Key::Alt, Key::Meta] {
            app.window().dispatch_event(WindowEvent::KeyPressed { text: key.into() });
            app.window().dispatch_event(WindowEvent::KeyReleased { text: key.into() });
            assert!(app.get_dict_open(), "bare modifier {key:?} must not dismiss YomitanPopup");
        }

        // Ctrl+Down / Ctrl+Up navigate popup without dismissing YomitanPopup.
        let navigated = Rc::new(Cell::new(0));
        let nav_sink = navigated.clone();
        app.on_navigate_popup(move |dir| nav_sink.set(dir));
        app.window().dispatch_event(WindowEvent::KeyPressed { text: Key::Control.into() });
        app.window().dispatch_event(WindowEvent::KeyPressed { text: Key::DownArrow.into() });
        app.window().dispatch_event(WindowEvent::KeyReleased { text: Key::DownArrow.into() });
        assert!(app.get_dict_open(), "Ctrl+Down must keep YomitanPopup open");
        assert_eq!(navigated.get(), 1);
        app.window().dispatch_event(WindowEvent::KeyPressed { text: Key::UpArrow.into() });
        app.window().dispatch_event(WindowEvent::KeyReleased { text: Key::UpArrow.into() });
        app.window().dispatch_event(WindowEvent::KeyReleased { text: Key::Control.into() });
        assert!(app.get_dict_open(), "Ctrl+Up must keep YomitanPopup open");
        assert_eq!(navigated.get(), -1);

        // When Anki is enabled and popup is open, Ctrl+D triggers add-anki-note without dismissing popup.
        app.set_anki_enabled(true);
        let added_note = Rc::new(Cell::new(-1));
        let added_sink = added_note.clone();
        app.on_add_anki_note(move |idx| added_sink.set(idx));
        app.window().dispatch_event(WindowEvent::KeyPressed { text: Key::Control.into() });
        app.window().dispatch_event(WindowEvent::KeyPressed { text: "d".into() });
        app.window().dispatch_event(WindowEvent::KeyReleased { text: "d".into() });
        app.window().dispatch_event(WindowEvent::KeyReleased { text: Key::Control.into() });
        assert!(app.get_dict_open());
        assert_eq!(added_note.get(), 0);

        // When popup is closed, Ctrl+D toggles DictionarySearchPanel.
        app.invoke_dismiss_definition();
        assert!(!app.get_dict_open());
        assert!(!app.get_dict_search_open());
        app.window().dispatch_event(WindowEvent::KeyPressed { text: Key::Control.into() });
        app.window().dispatch_event(WindowEvent::KeyPressed { text: "d".into() });
        app.window().dispatch_event(WindowEvent::KeyReleased { text: "d".into() });
        app.window().dispatch_event(WindowEvent::KeyReleased { text: Key::Control.into() });
        assert!(app.get_dict_search_open(), "Ctrl+D must toggle dictionary search panel");

        // Delete cycles OSC visibility mode (0 -> 1 -> 2 -> 0).
        assert_eq!(app.get_osc_visibility_mode(), 0);
        app.window().dispatch_event(WindowEvent::KeyPressed { text: Key::Delete.into() });
        app.window().dispatch_event(WindowEvent::KeyReleased { text: Key::Delete.into() });
        assert_eq!(app.get_osc_visibility_mode(), 1);
        assert!(app.get_osc_visible());
        app.window().dispatch_event(WindowEvent::KeyPressed { text: Key::Delete.into() });
        app.window().dispatch_event(WindowEvent::KeyReleased { text: Key::Delete.into() });
        assert_eq!(app.get_osc_visibility_mode(), 2);
        assert!(!app.get_osc_visible());
        app.window().dispatch_event(WindowEvent::KeyPressed { text: Key::Delete.into() });
        app.window().dispatch_event(WindowEvent::KeyReleased { text: Key::Delete.into() });
        assert_eq!(app.get_osc_visibility_mode(), 0);

        // Ctrl+F opens SubtitleDrawerPanel and focuses search.
        app.set_sub_drawer_open(false);
        app.window().dispatch_event(WindowEvent::KeyPressed { text: Key::Control.into() });
        app.window().dispatch_event(WindowEvent::KeyPressed { text: "f".into() });
        app.window().dispatch_event(WindowEvent::KeyReleased { text: "f".into() });
        app.window().dispatch_event(WindowEvent::KeyReleased { text: Key::Control.into() });
        assert!(app.get_sub_drawer_open(), "Ctrl+F must open SubtitleDrawerPanel");

        // F3 / Shift+F3 step forward and backward through subtitle search matches.
        app.invoke_search_subtitles("match".into());
        assert_eq!(app.get_subtitle_search_total_matches(), 2);
        assert_eq!(app.get_subtitle_search_current_match(), 1);
        assert_eq!(app.get_subtitle_search_focused_row(), 0);
        app.window().dispatch_event(WindowEvent::KeyPressed { text: Key::F3.into() });
        app.window().dispatch_event(WindowEvent::KeyReleased { text: Key::F3.into() });
        assert_eq!(app.get_subtitle_search_current_match(), 2);
        assert_eq!(app.get_subtitle_search_focused_row(), 2);
        app.window().dispatch_event(WindowEvent::KeyPressed { text: Key::Shift.into() });
        app.window().dispatch_event(WindowEvent::KeyPressed { text: Key::F3.into() });
        app.window().dispatch_event(WindowEvent::KeyReleased { text: Key::F3.into() });
        app.window().dispatch_event(WindowEvent::KeyReleased { text: Key::Shift.into() });
        assert_eq!(app.get_subtitle_search_current_match(), 1);
        assert_eq!(app.get_subtitle_search_focused_row(), 0);

        // Ctrl+Shift+C copies selected subtitle cues to clipboard.
        subtitles.borrow_mut().select_row(1, false);
        refresh_subtitle_marks(&rows, &subtitles.borrow());
        app.window().dispatch_event(WindowEvent::KeyPressed { text: Key::Control.into() });
        app.window().dispatch_event(WindowEvent::KeyPressed { text: Key::Shift.into() });
        app.window().dispatch_event(WindowEvent::KeyPressed { text: "C".into() });
        app.window().dispatch_event(WindowEvent::KeyReleased { text: "C".into() });
        app.window().dispatch_event(WindowEvent::KeyReleased { text: Key::Shift.into() });
        app.window().dispatch_event(WindowEvent::KeyReleased { text: Key::Control.into() });
        assert_eq!(app.get_copied_subtitle_text().as_str(), "Second middle line");
    }

    #[test]
    fn yomitan_popup_anchors_to_hovered_word_clamps_above_or_below_primary_subtitles_and_secondary_stays_at_top() {
        i_slint_backend_testing::init_no_event_loop();
        let app = MementoApp::new().unwrap();
        app.window().set_size(slint::LogicalSize::new(1280., 720.));
        app.set_subtitles_visible(true);
        app.set_active_sub_text("生前に世界中を旅行して集めた遺品が".into());
        app.set_secondary_sub_text("Top secondary subtitle line".into());
        let words = vec![
            SubtitleWord { text: "生前".into(), reading: "せいぜん".into(), query: "生前".into(), index: 0 },
            SubtitleWord { text: "に".into(), reading: "".into(), query: "に".into(), index: 1 },
            SubtitleWord { text: "世界".into(), reading: "せかい".into(), query: "世界".into(), index: 2 },
        ];
        let lines = subtitle_lines_for_app(&app, words.clone());
        app.set_active_sub_words(Rc::new(VecModel::from(words)).into());
        app.set_active_sub_lines(Rc::new(VecModel::from(lines)).into());
        app.set_dict_open(true);
        app.show().unwrap();

        // Secondary subtitles must sit at the top edge of the video viewport and not wrap short text.
        assert_eq!(app.get_secondary_subtitle_y(), 12.0);
        let short_sec_h = app.get_secondary_subtitle_height();
        app.set_secondary_sub_text("Hi".into());
        assert!(
            (app.get_secondary_subtitle_height() - short_sec_h).abs() <= 4.0,
            "Short secondary subtitle must stay single-line without wrapping (short_sec_h={short_sec_h}, two_char_h={})",
            app.get_secondary_subtitle_height()
        );

        // With default bottom subtitles (subtitle_offset = 0.045), YomitanPopup must sit ABOVE primary subtitles.
        let sub_top = app.get_primary_subtitle_y();
        let sub_bottom = sub_top + app.get_primary_subtitle_height();
        let popup_top = app.get_popup_y();
        let popup_bottom = popup_top + app.get_popup_height();
        assert!(
            popup_bottom <= sub_top,
            "popup above subtitles must not overlap primary subtitles (popup_bottom={popup_bottom}, sub_top={sub_top})"
        );

        // Horizontal anchoring shifts popup x when popup_offset_x changes, while clamping within viewport margins.
        let default_x = app.get_popup_x();
        app.set_popup_offset_x(180.0);
        assert!(app.get_popup_x() > default_x);
        app.set_popup_offset_x(5000.0);
        assert!(app.get_popup_x() + app.get_popup_width() <= 1280.0 - 20.0 + 0.5);
        app.set_popup_offset_x(-5000.0);
        assert!(app.get_popup_x() >= 20.0 - 0.5);
        app.set_popup_offset_x(0.0);

        // When primary subtitles are moved near the top of the video (e.g. subtitle_offset = 0.85),
        // YomitanPopup must flip BELOW primary subtitles so it NEVER covers active subtitles!
        app.set_subtitle_offset(0.85);
        let top_sub_y = app.get_primary_subtitle_y();
        let top_sub_bottom = top_sub_y + app.get_primary_subtitle_height();
        let flipped_popup_y = app.get_popup_y();
        assert!(
            flipped_popup_y >= top_sub_bottom,
            "popup must flip below top-positioned primary subtitles (flipped_popup_y={flipped_popup_y}, top_sub_bottom={top_sub_bottom})"
        );
        let _ = sub_bottom;
    }

    #[test]
    fn subtitle_drawer_search_match_counter_navigation_tabs_copy_and_resizable_split() {
        i_slint_backend_testing::init_no_event_loop();
        let app = MementoApp::new().unwrap();
        app.window().set_size(slint::LogicalSize::new(1280., 720.));
        app.set_sub_drawer_open(true);
        app.set_dict_search_open(true);
        let subtitles = Rc::new(RefCell::new(SubtitleController::new()));
        subtitles.borrow_mut().load_cues(vec![
            controller::subtitles::create_cue("Alpha one", 1., 2.),
            controller::subtitles::create_cue("Beta two", 3., 4.),
            controller::subtitles::create_cue("Alpha three", 5., 6.),
        ]);
        subtitles.borrow_mut().ensure_track(2);
        subtitles.borrow_mut().set_track_cues(
            2,
            vec![
                controller::subtitles::create_cue("Secondary cue A", 1., 2.),
                controller::subtitles::create_cue("Secondary cue B", 3., 4.),
            ],
        );
        subtitles.borrow_mut().set_secondary_sid(2);
        app.set_secondary_sid(2);

        let primary_rows = Rc::new(VecModel::<SubEntry>::default());
        let secondary_rows = Rc::new(VecModel::<SubEntry>::default());
        app.set_subtitle_entries(primary_rows.clone().into());
        app.set_secondary_subtitle_entries(secondary_rows.clone().into());
        refresh_subtitle_list(&app, &primary_rows, &subtitles.borrow());
        refresh_secondary_subtitle_list(&app, &secondary_rows, &subtitles.borrow());
        install_subtitle_selection(&app, subtitles.clone(), primary_rows.clone());
        app.show().unwrap();

        // Both DictionarySearchPanel and SubtitleDrawerPanel stay open in a resizable split.
        assert!(app.get_dict_search_open() && app.get_sub_drawer_open());
        app.set_sidebar_width(400.0);
        app.set_sidebar_split_ratio(0.35);
        assert_eq!(app.get_sidebar_width(), 400.0);
        assert!((app.get_sidebar_split_ratio() - 0.35).abs() < 1e-4);

        // Search on Primary tab (tab 0) counts matches and navigates with wrap-around.
        app.invoke_search_subtitles("Alpha".into());
        assert_eq!(app.get_subtitle_search_total_matches(), 2);
        assert_eq!(app.get_subtitle_search_current_match(), 1);
        assert_eq!(app.get_subtitle_search_focused_row(), 0);
        app.invoke_next_subtitle_search_match();
        assert_eq!(app.get_subtitle_search_current_match(), 2);
        assert_eq!(app.get_subtitle_search_focused_row(), 2);
        app.invoke_next_subtitle_search_match();
        assert_eq!(app.get_subtitle_search_current_match(), 1);
        assert_eq!(app.get_subtitle_search_focused_row(), 0);

        // Switching to Secondary tab (tab 1) re-evaluates search against secondary cues.
        app.set_subtitle_drawer_tab(1);
        app.invoke_subtitle_drawer_tab_changed(1);
        assert_eq!(app.get_subtitle_search_total_matches(), 0);
        app.invoke_search_subtitles("Secondary".into());
        assert_eq!(app.get_subtitle_search_total_matches(), 2);
        assert_eq!(app.get_subtitle_search_current_match(), 1);
        assert_eq!(app.get_subtitle_search_focused_row(), 0);
        app.invoke_prev_subtitle_search_match();
        assert_eq!(app.get_subtitle_search_current_match(), 2);
        assert_eq!(app.get_subtitle_search_focused_row(), 1);
    }

    #[test]
    fn about_modal_click_shield_url_modal_volume_max_mute_chapters_and_recent_titles() {
        i_slint_backend_testing::init_no_event_loop();
        let app = MementoApp::new().unwrap();
        app.window().set_size(slint::LogicalSize::new(1280., 720.));
        app.set_about_open(true);
        app.show().unwrap();

        let click = |x, y| {
            let position = slint::LogicalPosition::new(x, y);
            app.window().dispatch_event(WindowEvent::PointerPressed {
                position,
                button: PointerEventButton::Left,
            });
            app.window().dispatch_event(WindowEvent::PointerReleased {
                position,
                button: PointerEventButton::Left,
            });
        };

        // Clicking inside the About dialog card (center of 1280x720) must NOT dismiss it.
        click(640., 360.);
        assert!(app.get_about_open(), "About modal click-shield must prevent inner clicks from closing");
        // Clicking the backdrop outside the card closes About modal.
        click(50., 50.);
        assert!(!app.get_about_open(), "Clicking outside About card must close modal");

        // Volume boost (>100% volume-max) and mute state.
        app.set_volume_max(150.0);
        app.set_volume(135.0);
        app.set_is_muted(true);
        assert_eq!(app.get_volume_max(), 150.0);
        assert_eq!(app.get_volume(), 135.0);
        assert!(app.get_is_muted());

        // Chapter marks & timeline hover chapter lookup.
        let marks = chapter_marks_for_duration(&[0.0, 90.0, 300.0], 600.0);
        assert_eq!(marks.len(), 3);
        assert_eq!(marks[1].title.as_str(), "Chapter 2");
        app.set_chapter_marks(Rc::new(VecModel::from(marks)).into());
        assert_eq!(chapter_title_at_time(&app, 45.0), "Chapter 1");
        assert_eq!(chapter_title_at_time(&app, 120.0), "Chapter 2");
        assert_eq!(chapter_title_at_time(&app, 450.0), "Chapter 3");

        // Recent file label includes media-title alongside path (#338).
        assert_eq!(
            format_recent_file_label("Frieren Episode 1", "/home/user/Videos/ep01.mkv"),
            "Frieren Episode 1 — /home/user/Videos/ep01.mkv"
        );
        assert_eq!(
            format_recent_file_label("", "/home/user/Videos/ep01.mkv"),
            "ep01.mkv — /home/user/Videos/ep01.mkv"
        );
    }

    #[test]
    fn subtitle_drawer_auto_scrolls_on_active_cue_advance_and_respects_user_scrolling_and_hover_pause_release() {
        i_slint_backend_testing::init_no_event_loop();
        let app = MementoApp::new().unwrap();
        app.window().set_size(slint::LogicalSize::new(1280., 720.));
        app.set_sub_drawer_open(true);
        let subtitles = Rc::new(RefCell::new(SubtitleController::new()));
        let mut cues = Vec::new();
        for i in 0..30 {
            let start = (i * 2 + 1) as f64;
            let text = if i < 5 {
                format!("Multi-line wrapped subtitle cue #{i} with extra long text that wraps across multiple lines in the subtitle drawer panel")
            } else {
                format!("Cue line #{i}")
            };
            cues.push(controller::subtitles::create_cue(&text, start, start + 1.5));
        }
        subtitles.borrow_mut().load_cues(cues);
        let rows = Rc::new(VecModel::<SubEntry>::default());
        app.set_subtitle_entries(rows.clone().into());
        install_subtitle_selection(&app, subtitles.clone(), rows.clone());
        app.show().unwrap();

        assert_eq!(app.get_subtitle_drawer_viewport_y(), 0.0);

        // Advancing active playback cue to row 22 while user is NOT scrolling auto-scrolls the drawer.
        subtitles.borrow_mut().update_position(45.5);
        refresh_subtitle_list(&app, &rows, &subtitles.borrow());
        let scrolled_y = app.get_subtitle_drawer_viewport_y();
        assert!(
            scrolled_y < -100.0,
            "Active cue advance must auto-scroll SubtitleDrawerPanel (viewport_y={scrolled_y})"
        );

        // When user-scrolling is active, advancing the playback cue must NOT overwrite the user's scroll position.
        app.set_subtitle_drawer_user_scrolling(true);
        subtitles.borrow_mut().update_position(1.5);
        refresh_subtitle_list(&app, &rows, &subtitles.borrow());
        assert_eq!(
            app.get_subtitle_drawer_viewport_y(),
            scrolled_y,
            "Auto-scroll must not fight the user while user_scrolling is active"
        );

        // Clearing user_scrolling allows active-cue auto-scroll to resume.
        app.set_subtitle_drawer_user_scrolling(false);
        subtitles.borrow_mut().update_position(1.5);
        refresh_subtitle_list(&app, &rows, &subtitles.borrow());
        assert_eq!(app.get_subtitle_drawer_viewport_y(), 0.0);

        // Hover-pause latches across subtitle -> YomitanPopup and releases when moving onto OSC / menu bar / sidebar.
        let hover_state = Rc::new(Cell::new(false));
        let hover_sink = hover_state.clone();
        app.on_subtitle_hover_changed(move |hovering| hover_sink.set(hovering));
        app.set_subtitles_visible(true);
        app.set_active_sub_text("生前に世界中".into());
        let words = vec![
            SubtitleWord { text: "生前".into(), reading: "せいぜん".into(), query: "生前".into(), index: 0 },
        ];
        let lines = subtitle_lines_for_app(&app, words.clone());
        app.set_active_sub_words(Rc::new(VecModel::from(words)).into());
        app.set_active_sub_lines(Rc::new(VecModel::from(lines)).into());
        set_dictionary_rows(
            &app,
            vec![DictResult {
                index: 0,
                expression: "生前".into(),
                reading: "せいぜん".into(),
                glossary: "during one's lifetime".into(),
                dict_name: "JMdict".into(),
                selected: true,
                term_start: true,
                term_end: true,
                ..Default::default()
            }],
        );
        app.set_dict_open(true);

        // Move pointer over primary subtitle box -> hover-pause activates.
        let sub_y = 30.0 + app.get_primary_subtitle_y() + 10.0;
        app.window().dispatch_event(WindowEvent::PointerMoved {
            position: slint::LogicalPosition::new(470., sub_y),
        });
        assert!(hover_state.get(), "Hovering primary subtitle must activate hover-pause");

        // Move pointer into YomitanPopup -> hover-pause stays active.
        let popup_y = 30.0 + app.get_popup_y() + 40.0;
        let popup_x = app.get_popup_x() + 40.0;
        app.window().dispatch_event(WindowEvent::PointerMoved {
            position: slint::LogicalPosition::new(popup_x, popup_y),
        });
        assert!(hover_state.get(), "Hovering YomitanPopup must keep hover-pause active");

        // Move pointer directly from YomitanPopup onto the top menu bar (or sidebar/OSC) -> hover-pause releases!
        app.window().dispatch_event(WindowEvent::PointerMoved {
            position: slint::LogicalPosition::new(30., 12.),
        });
        assert!(
            !hover_state.get(),
            "Moving pointer from YomitanPopup onto menu bar / OSC / sidebar must release hover-pause"
        );
    }

    #[test]
    fn spawn_and_reap_leaves_zero_defunct_zombies_and_browse_targets_normalize() {
        let pid = spawn_and_reap(Command::new("true")).expect("should spawn /usr/bin/true");
        #[cfg(target_os = "linux")]
        {
            let stat_path = PathBuf::from(format!("/proc/{pid}/stat"));
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while stat_path.exists() && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            if let Ok(stat) = std::fs::read_to_string(&stat_path) {
                let state = stat.rsplit_once(')').and_then(|(_, rest)| rest.split_whitespace().next());
                assert_ne!(state, Some("Z"), "spawned child pid {pid} became a <defunct> zombie: {stat}");
            }
        }

        let cmd = open_directory_command(Path::new("/tmp/memento-cfg"));
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        assert_eq!(cmd.get_program(), "xdg-open");

        assert_eq!(
            controller::anki::normalize_anki_browse_target("1669909161234").as_deref(),
            Some("nid:1669909161234")
        );
        assert_eq!(
            controller::anki::normalize_anki_browse_target("nid:123 or nid:456").as_deref(),
            Some("nid:123 or nid:456")
        );
        assert_eq!(controller::anki::normalize_anki_browse_target(""), None);
        assert_eq!(controller::anki::normalize_anki_browse_target("0"), None);
    }

    #[test]
    fn apply_dictionary_order_wires_search_ipadic_matcher_setting() {
        let cfg = ConfigController::new();
        let mut dict = DictController::new();
        assert!(!dict.mecab_enabled());

        assert_eq!(
            mecab_matcher_enabled(&cfg),
            raw::search::matcher::MECAB_IPADIC_DEFAULT
        );
        apply_dictionary_order(&cfg, &mut dict);
        assert_eq!(dict.mecab_enabled(), raw::search::matcher::MECAB_IPADIC_DEFAULT);

        let key = format!("{}/{}", raw::search::GROUP, raw::search::matcher::MECAB_IPADIC);
        cfg.edit_setting(&key, "false").unwrap();
        assert!(!mecab_matcher_enabled(&cfg));
        apply_dictionary_order(&cfg, &mut dict);
        assert!(!dict.mecab_enabled());

        cfg.edit_setting(&key, "true").unwrap();
        assert!(mecab_matcher_enabled(&cfg));
        apply_dictionary_order(&cfg, &mut dict);
        assert!(dict.mecab_enabled());
    }

    #[test]
    fn apply_dictionary_order_wires_exact_deconj_and_text_filters() {
        let cfg = ConfigController::new();
        let mut dict = DictController::new();
        let mut noun = controller::dict::SearchHit::new("猫", "ねこ", "cat", "n", 10, false);
        noun.rules = "n".to_owned();
        let mut verb = controller::dict::SearchHit::new("食べる", "たべる", "to eat", "v1", 10, false);
        verb.rules = "v1".to_owned();
        dict.add_entries([noun, verb]);

        cfg.edit_setting("search/ipadic-matcher", "false").unwrap();
        cfg.edit_setting("search/exact-matcher", "false").unwrap();
        cfg.edit_setting("search/deconj-matcher", "true").unwrap();
        cfg.edit_setting("search/remove-regex", "（[^）]*）").unwrap();
        cfg.edit_setting("search/replace-lines", "true").unwrap();
        cfg.edit_setting("search/replace-with", "").unwrap();
        apply_dictionary_order(&cfg, &mut dict);

        assert!(!dict.exact_enabled());
        assert!(dict.deconj_enabled());
        assert!(!dict.mecab_enabled());
        assert_eq!(dict.remove_regex_pattern(), "（[^）]*）");
        assert!(dict.replace_lines());
        assert_eq!(dict.replace_lines_with(), "");
        assert!(dict.search("猫").is_empty());
        assert_eq!(
            dict.search("（笑）食べ\nた").first().map(|h| h.expression.as_str()),
            Some("食べる")
        );

        cfg.edit_setting("search/exact-matcher", "true").unwrap();
        cfg.edit_setting("search/deconj-matcher", "false").unwrap();
        apply_dictionary_order(&cfg, &mut dict);
        assert!(dict.exact_enabled());
        assert!(!dict.deconj_enabled());
        assert_eq!(dict.search("（笑）猫").first().map(|h| h.expression.as_str()), Some("猫"));
        assert!(dict.search("食べた").is_empty());
    }

    #[test]
    fn subtitle_list_timestamps_toggle_ignore_whitespace_auto_seek_and_styling_binding() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::TempDir::new().unwrap();
        let cfg = Rc::new(ConfigController::new());
        cfg.edit_setting("interface/sub-list-timestamps", "true").unwrap();
        cfg.edit_setting("subtitle-list/ignore-whitespace", "false").unwrap();
        cfg.edit_setting("subtitle-list/auto-seek", "true").unwrap();
        cfg.edit_setting("interface/sub-list-background-color", "#FF112233").unwrap();
        cfg.edit_setting("interface/sub-list-text-color", "#FFCCDDEE").unwrap();
        cfg.edit_setting("interface/sub-list-hover-color", "#FF223344").unwrap();
        cfg.edit_setting("interface/sub-list-selected-color", "#FF334455").unwrap();
        cfg.edit_setting("interface/sub-list-selected-hover-color", "#FF445566").unwrap();
        cfg.edit_setting("interface/sub-list-primary-font", "Noto Sans CJK JP,18,-1,5,75,1,0,0,0,0")
            .unwrap();
        cfg.edit_setting("interface/sub-list-secondary-font", "Monospace,14,-1,5,50,0,0,0,0,0")
            .unwrap();

        let app = MementoApp::new().unwrap();
        app.window().set_size(slint::LogicalSize::new(1280., 720.));
        app.set_sub_drawer_open(true);
        refresh_player_preferences(&app, &cfg);

        assert!(app.get_sub_list_show_timestamps());
        assert!(!app.get_subtitle_search_ignore_whitespace());
        assert!(app.get_subtitle_search_auto_seek());
        assert_eq!(app.get_sub_list_bg_color(), slint::Color::from_argb_u8(255, 0x11, 0x22, 0x33));
        assert_eq!(app.get_sub_list_text_color(), slint::Color::from_argb_u8(255, 0xCC, 0xDD, 0xEE));
        assert_eq!(app.get_sub_list_hover_color(), slint::Color::from_argb_u8(255, 0x22, 0x33, 0x44));
        assert_eq!(app.get_sub_list_selected_color(), slint::Color::from_argb_u8(255, 0x33, 0x44, 0x55));
        assert_eq!(
            app.get_sub_list_selected_hover_color(),
            slint::Color::from_argb_u8(255, 0x44, 0x55, 0x66)
        );
        assert_eq!(app.get_sub_list_primary_font_family().as_str(), "Noto Sans CJK JP");
        assert_eq!(app.get_sub_list_primary_font_size(), 18.0);
        assert!(app.get_sub_list_primary_font_bold());
        assert!(app.get_sub_list_primary_font_italic());
        assert_eq!(app.get_sub_list_secondary_font_family().as_str(), "Monospace");
        assert_eq!(app.get_sub_list_secondary_font_size(), 14.0);
        assert!(!app.get_sub_list_secondary_font_bold());
        assert!(!app.get_sub_list_secondary_font_italic());

        // Wire timestamps toggle callback -> persistence into memento.conf.
        let ts_cfg = cfg.clone();
        let ts_dir = dir.path().to_path_buf();
        app.on_sub_list_show_timestamps_toggled(move |show| {
            let key = format!(
                "{}/{}",
                raw::interface::GROUP,
                raw::interface::subtitle_list::TIMESTAMPS
            );
            ts_cfg
                .persist_single_setting(&key, if show { "true" } else { "false" }, &ts_dir, "memento.conf")
                .unwrap();
        });
        app.show().unwrap();
        // Click the inline "Time" toggle pill at the top-right of the 360px drawer header (x ~ 1240, y ~ 54).
        let pos = slint::LogicalPosition::new(1240.0, 54.0);
        app.window().dispatch_event(WindowEvent::PointerPressed {
            position: pos,
            button: PointerEventButton::Left,
        });
        app.window().dispatch_event(WindowEvent::PointerReleased {
            position: pos,
            button: PointerEventButton::Left,
        });
        assert!(!app.get_sub_list_show_timestamps(), "clicking Time pill must toggle timestamps off");
        let reloaded = ConfigController::load(dir.path(), "memento.conf").unwrap();
        assert_eq!(
            reloaded.effective_setting(raw::interface::GROUP, raw::interface::subtitle_list::TIMESTAMPS),
            Some(Value::Bool(false))
        );

        // Subtitle search respects live `ignore-whitespace` and `auto-seek`.
        let subtitles = Rc::new(RefCell::new(SubtitleController::new()));
        subtitles.borrow_mut().load_cues(vec![
            controller::subtitles::create_cue("Alpha one", 12.0, 14.0),
            controller::subtitles::create_cue("Beta two", 25.0, 27.0),
            controller::subtitles::create_cue("Alpha   three", 41.0, 43.0),
        ]);
        let rows = Rc::new(VecModel::<SubEntry>::default());
        app.set_subtitle_entries(rows.clone().into());
        refresh_subtitle_list(&app, &rows, &subtitles.borrow());
        install_subtitle_selection(&app, subtitles.clone(), rows.clone());

        let sought = Rc::new(RefCell::new(Vec::<f32>::new()));
        let sought_sink = sought.clone();
        app.on_seek_to(move |sec| sought_sink.borrow_mut().push(sec));

        // With `ignore-whitespace = false`, "Alphaone" does NOT match "Alpha one".
        app.invoke_search_subtitles("Alphaone".into());
        assert_eq!(app.get_subtitle_search_total_matches(), 0);
        assert!(sought.borrow().is_empty());

        // With `ignore-whitespace = true` and `auto-seek = true`, search & next/prev auto-seek!
        app.set_subtitle_search_ignore_whitespace(true);
        app.invoke_search_subtitles("Alpha".into());
        assert_eq!(app.get_subtitle_search_total_matches(), 2);
        assert_eq!(sought.borrow().as_slice(), &[12.0]);
        app.invoke_next_subtitle_search_match();
        assert_eq!(sought.borrow().as_slice(), &[12.0, 41.0]);
        app.invoke_prev_subtitle_search_match();
        assert_eq!(sought.borrow().as_slice(), &[12.0, 41.0, 12.0]);

        // Turning `auto-seek` off stops seeking on match navigation.
        app.set_subtitle_search_auto_seek(false);
        app.invoke_next_subtitle_search_match();
        assert_eq!(sought.borrow().as_slice(), &[12.0, 41.0, 12.0]);
        app.hide().unwrap();
    }

    #[test]
    fn auto_play_audio_triggers_on_first_term_result_when_enabled() {
        i_slint_backend_testing::init_no_event_loop();
        let app = MementoApp::new().unwrap();
        let cfg = ConfigController::new();
        cfg.edit_setting("search/auto-play-audio", "true").unwrap();
        refresh_player_preferences(&app, &cfg);
        assert!(app.get_auto_play_audio_enabled());

        let played = Rc::new(RefCell::new(Vec::<(String, String)>::new()));
        let played_sink = played.clone();
        app.on_play_audio_for_word(move |expr, reading| {
            played_sink.borrow_mut().push((expr.to_string(), reading.to_string()));
        });

        let term = controller::dict::SearchHit::new("食べる", "たべる", "to eat", "v1", 10, false);
        let kanji = controller::dict::SearchHit::new("食", "しょく", "eat", "kanji", 1, true);
        display_word_lookup(
            &app,
            WordLookupResult {
                hits: vec![kanji.clone(), term.clone()],
                error: None,
                database_open: true,
                matched_units: 3,
                headword: "食べる".into(),
                reading: "たべる".into(),
                pitch: String::new(),
                freq: String::new(),
                jpdb: String::new(),
            },
            "食べる",
            &[],
        );
        assert_eq!(
            played.borrow().as_slice(),
            &[("食べる".to_owned(), "たべる".to_owned())]
        );

        // Disabled setting must not auto-play.
        played.borrow_mut().clear();
        cfg.edit_setting("search/auto-play-audio", "false").unwrap();
        refresh_player_preferences(&app, &cfg);
        assert!(!app.get_auto_play_audio_enabled());
        display_word_lookup(
            &app,
            WordLookupResult {
                hits: vec![term],
                error: None,
                database_open: true,
                matched_units: 3,
                headword: "食べる".into(),
                reading: "たべる".into(),
                pitch: String::new(),
                freq: String::new(),
                jpdb: String::new(),
            },
            "食べる",
            &[],
        );
        assert!(played.borrow().is_empty());
    }

    #[test]
    fn cursor_reveal_zones_osc_fade_and_osc_mpv_cursor_bindings_work_in_headless_ui() {
        i_slint_backend_testing::init_no_event_loop();
        let cfg = ConfigController::new();
        cfg.edit_setting("behavior/osc-fade", "0").unwrap();
        cfg.edit_setting("behavior/osc-mpv-cursor", "true").unwrap();
        cfg.edit_setting("behavior/subtitle-cursor-show", "true").unwrap();
        cfg.edit_setting("behavior/subtitle-cursor-show-secondary", "true").unwrap();

        let app = MementoApp::new().unwrap();
        app.window().set_size(slint::LogicalSize::new(1280., 720.));
        refresh_player_preferences(&app, &cfg);
        assert_eq!(app.get_osc_fade_ms(), 0);
        assert!(app.get_osc_mpv_cursor());
        assert!(app.get_subtitle_cursor_show_enabled());
        assert!(app.get_secondary_subtitle_cursor_show_enabled());

        // With `osc-fade-ms = 0`, toggling `osc-visible` switches opacity immediately without animation lag.
        app.set_osc_visible(false);
        assert_eq!(app.get_osc_opacity(), 0.0);
        assert_eq!(app.get_menu_bar_opacity(), 0.0);
        app.set_osc_visible(true);
        assert_eq!(app.get_osc_opacity(), 1.0);
        assert_eq!(app.get_menu_bar_opacity(), 1.0);

        // Even when subtitles are hidden (`!subtitles-visible` and `hide-memento-while-playing && is-playing`),
        // moving the pointer into the bottom 20% reveals primary subtitles, and top 20% reveals secondary subtitles.
        app.set_active_sub_text("Primary line".into());
        app.set_secondary_sub_text("Secondary line".into());
        app.set_subtitles_visible(false);
        app.set_hide_memento_while_playing(true);
        app.set_is_playing(true);
        app.show().unwrap();

        // Pointer in middle of video (y = 360px, video area starts at y = 30px): both hidden.
        app.window().dispatch_event(WindowEvent::PointerMoved {
            position: slint::LogicalPosition::new(640., 360.),
        });
        assert!(!app.get_cursor_in_top_sub_zone());
        assert!(!app.get_cursor_in_bottom_sub_zone());
        assert!(!app.get_primary_subtitle_effective_visible());
        assert!(!app.get_secondary_subtitle_effective_visible());

        // Pointer in bottom 20% of video above OSC (y = 610px -> 580/690 = 84%): primary revealed, secondary hidden.
        app.window().dispatch_event(WindowEvent::PointerMoved {
            position: slint::LogicalPosition::new(640., 610.),
        });
        assert!(app.get_cursor_in_bottom_sub_zone());
        assert!(!app.get_cursor_in_top_sub_zone());
        assert!(app.get_primary_subtitle_effective_visible());
        assert!(!app.get_secondary_subtitle_effective_visible());

        // Pointer hovering the bottom OSC bar itself (y = 660px with OSC visible) keeps primary revealed.
        app.window().dispatch_event(WindowEvent::PointerMoved {
            position: slint::LogicalPosition::new(640., 660.),
        });
        assert!(app.get_primary_subtitle_effective_visible());
        assert!(!app.get_secondary_subtitle_effective_visible());

        // With OSC hidden during playback, pointer at y = 660px (91%) lands on video-motion bottom zone.
        app.set_osc_visible(false);
        app.window().dispatch_event(WindowEvent::PointerMoved {
            position: slint::LogicalPosition::new(640., 660.),
        });
        assert!(app.get_cursor_in_bottom_sub_zone());
        assert!(app.get_primary_subtitle_effective_visible());
        assert!(!app.get_secondary_subtitle_effective_visible());

        // Pointer in top 20% of video (y = 80px): secondary revealed, primary hidden.
        app.set_osc_visible(true);
        app.window().dispatch_event(WindowEvent::PointerMoved {
            position: slint::LogicalPosition::new(640., 80.),
        });
        assert!(app.get_cursor_in_top_sub_zone());
        assert!(!app.get_cursor_in_bottom_sub_zone());
        assert!(!app.get_primary_subtitle_effective_visible());
        assert!(app.get_secondary_subtitle_effective_visible());
        app.hide().unwrap();
    }

    #[test]
    fn file_open_dir_resolution_and_update_dialog_flow() {
        let root = tempfile::TempDir::new().unwrap();
        let home = root.path().join("home");
        let current = root.path().join("cwd");
        let videos = home.join("Videos");
        let documents = home.join("Documents");
        let custom = root.path().join("My Anime");
        for d in [&home, &current, &videos, &documents, &custom] {
            std::fs::create_dir_all(d).unwrap();
        }

        // 0=Current, 1=Home, 2=Movies, 3=Documents, 4=Custom (plain path, file:// URI, ~/path, missing fallback)
        assert_eq!(
            resolve_file_open_dir(0, "", Some(&home), Some(&current), Some(&videos), Some(&documents)),
            Some(current.clone())
        );
        assert_eq!(
            resolve_file_open_dir(1, "", Some(&home), Some(&current), Some(&videos), Some(&documents)),
            Some(home.clone())
        );
        assert_eq!(
            resolve_file_open_dir(2, "", Some(&home), Some(&current), Some(&videos), Some(&documents)),
            Some(videos.clone())
        );
        assert_eq!(
            resolve_file_open_dir(3, "", Some(&home), Some(&current), Some(&videos), Some(&documents)),
            Some(documents.clone())
        );
        assert_eq!(
            resolve_file_open_dir(
                4,
                custom.to_str().unwrap(),
                Some(&home),
                Some(&current),
                Some(&videos),
                Some(&documents),
            ),
            Some(custom.clone())
        );
        let file_uri = format!("file://{}", custom.to_str().unwrap().replace(' ', "%20"));
        assert_eq!(
            resolve_file_open_dir(4, &file_uri, Some(&home), Some(&current), Some(&videos), Some(&documents)),
            Some(custom.clone())
        );
        assert_eq!(
            resolve_file_open_dir(4, "~/Videos", Some(&home), Some(&current), None, None),
            Some(videos.clone())
        );
        assert_eq!(
            resolve_file_open_dir(
                4,
                "/nonexistent/memento/dir/xyz",
                Some(&home),
                Some(&current),
                Some(&videos),
                Some(&documents),
            ),
            Some(home.clone()),
            "missing custom directory must fall back cleanly to home"
        );

        // Startup update check decision & UpdateDialog modal outcomes.
        let cfg = ConfigController::new();
        assert_eq!(determine_startup_update_action(&cfg), StartupUpdateAction::ShowOptInPrompt);
        cfg.edit_setting("internal/auto-update-opt-in-shown", "true").unwrap();
        assert_eq!(determine_startup_update_action(&cfg), StartupUpdateAction::Nothing);
        cfg.edit_setting("application/auto-update-check", "true").unwrap();
        assert_eq!(determine_startup_update_action(&cfg), StartupUpdateAction::RunSilentCheck);

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
        assert_eq!(app.get_update_dialog_title().as_str(), "New Version Available");

        // Manual up-to-date check opens modal with "Up to Date".
        app.set_update_dialog_open(false);
        apply_update_check_outcome(&app, "2.0.2", Ok("v2.0.2".to_owned()), false);
        assert!(app.get_update_dialog_open());
        assert_eq!(app.get_update_dialog_title().as_str(), "Up to Date");
        assert!(app.get_update_dialog_release_tag().is_empty());
    }
}

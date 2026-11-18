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
mod video_gl;
mod graphics_device;

pub use ui::{
    DictResult, InstalledDictionaryRow, MementoApp, MementoPalette, PlayerTrack,
    PitchGraph, PitchMora, PopupAudioSource, PopupFrequency, PopupGlossaryBlock,
    AnkiHelpWindow, SettingsWindow, SubEntry, SubtitleLine, SubtitleWord,
};
use ui::{publish_active_subtitle, dictionary_result_rows, set_dictionary_rows, popup_pitch_graphs, subtitle_lines_for_app};
#[cfg(test)]
use ui::{subtitle_lines_from_words, subtitle_token_width};
use controller::anki::AnkiController;
use controller::config::ConfigController;
use controller::dict::DictController;
use controller::player::{
    clear_primary_subtitle, compute_video_render_size, effective_media_title,
    refresh_subtitle_list, refresh_subtitle_marks, LivePlayerCoordinator, MouseInputForwarder,
};
#[cfg(test)]
use controller::player::{
    clear_subtitle_marks_on_advance, sync_player_tracks as sync_player_tracks_with_asr,
};
use controller::subtitles::{SubtitleController, ExternalSubtitleLoad};
#[cfg(test)]
use controller::subtitles::{MemorySubtitleCue, ASR_TRACK_ID};
use controller::dictmanager::{DictionaryManager, InstalledDictionary};
use memento_config::Value;
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

fn record_recent_media(
    app: &MementoApp,
    config: &ConfigController,
    cache_dir: &Path,
    path: &str,
) -> Result<(), String> {
    config.add_recent_file(path)?;
    config.save(cache_dir, "memento.conf")?;
    refresh_recent_files(app, config);
    Ok(())
}

fn apply_dictionary_order(config: &ConfigController, dictionary: &mut DictController) {
    if let Some(Value::Ints(order)) =
        config.effective_setting(raw::dictionary::GROUP, raw::dictionary::ORDER)
    {
        dictionary.set_order(&order);
    }
}

fn ordered_dictionaries(
    mut dictionaries: Vec<InstalledDictionary>,
    config: &ConfigController,
) -> Vec<InstalledDictionary> {
    let order = match config.effective_setting(raw::dictionary::GROUP, raw::dictionary::ORDER) {
        Some(Value::Ints(order)) => order,
        _ => Vec::new(),
    };
    dictionaries.sort_by_key(|dictionary| {
        (
            order.iter().position(|id| *id == dictionary.id).is_none(),
            order
                .iter()
                .position(|id| *id == dictionary.id)
                .unwrap_or(usize::MAX),
            dictionary.id,
        )
    });
    dictionaries
}

fn dictionary_settings_rows(
    manager: &DictionaryManager,
    config: &ConfigController,
) -> Result<Vec<InstalledDictionaryRow>, String> {
    ordered_dictionaries(
        manager.list_dictionaries().map_err(|error| error.to_string())?,
        config,
    )
    .into_iter()
    .map(|dictionary| {
        Ok(InstalledDictionaryRow {
            id: i32::try_from(dictionary.id)
                .map_err(|_| format!("dictionary ID {} is outside Slint's integer range", dictionary.id))?,
            title: dictionary.title.into(),
            disabled: dictionary.disabled,
        })
    })
    .collect()
}

fn refresh_dictionary_settings(
    window: &SettingsWindow,
    manager: Option<&DictionaryManager>,
    config: &ConfigController,
) {
    let rows = manager.map_or_else(
        || Ok(Vec::new()),
        |manager| dictionary_settings_rows(manager, config),
    );
    match rows {
        Ok(rows) => {
            window.set_installed_dictionaries(Rc::new(VecModel::from(rows)).into());
        }
        Err(error) => window.set_settings_status(format!("Dictionary list: {error}").into()),
    }
}

fn save_dictionary_order(
    config: &ConfigController,
    dictionaries: &[InstalledDictionary],
    cache_dir: &Path,
    anki_json_path: &Path,
) -> Result<(), String> {
    let order = dictionaries
        .iter()
        .map(|dictionary| dictionary.id.to_string())
        .collect::<Vec<_>>()
        .join(",");
    config.edit_setting("dictionary/order", &order)?;
    config.apply_settings(4, cache_dir, "memento.conf", anki_json_path)
}

fn mutate_dictionary_database(
    dictionary: &Rc<RefCell<DictController>>,
    database_path: &Path,
    config: &ConfigController,
    operation: impl FnOnce() -> Result<Vec<InstalledDictionary>, controller::dictmanager::DictionaryManagerError>,
) -> Result<Vec<InstalledDictionary>, String> {
    *dictionary.borrow_mut() = DictController::new();
    let operation = operation().map_err(|error| error.to_string());
    let reopened = DictController::open(database_path)
        .map(|mut replacement| {
            apply_dictionary_order(config, &mut replacement);
            *dictionary.borrow_mut() = replacement;
        })
        .map_err(|error| format!("Could not reopen dictionary database: {error}"));
    match (operation, reopened) {
        (Ok(dictionaries), Ok(())) => Ok(dictionaries),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Err(operation), Err(reopen)) => Err(format!("{operation}; {reopen}")),
    }
}

fn move_dictionary_order(
    manager: &DictionaryManager,
    config: &ConfigController,
    dictionary: &Rc<RefCell<DictController>>,
    cache_dir: &Path,
    anki_json_path: &Path,
    id: i32,
    direction: i32,
) -> Result<(), String> {
    let mut dictionaries = ordered_dictionaries(
        manager.list_dictionaries().map_err(|error| error.to_string())?,
        config,
    );
    let index = dictionaries
        .iter()
        .position(|dictionary| dictionary.id == i64::from(id))
        .ok_or_else(|| format!("dictionary {id} is no longer installed"))?;
    let target = index as isize + direction.signum() as isize;
    if !(0..dictionaries.len() as isize).contains(&target) {
        return Ok(());
    }
    dictionaries.swap(index, target as usize);
    save_dictionary_order(config, &dictionaries, cache_dir, anki_json_path)?;
    apply_dictionary_order(config, &mut dictionary.borrow_mut());
    Ok(())
}

fn finish_dictionary_change(
    window: &SettingsWindow,
    manager: &DictionaryManager,
    config: &ConfigController,
    dictionary: &Rc<RefCell<DictController>>,
    cache_dir: &Path,
    anki_json_path: &Path,
    dictionaries: Vec<InstalledDictionary>,
    persist_order: bool,
) {
    let dictionaries = ordered_dictionaries(dictionaries, config);
    if persist_order {
        if let Err(error) =
            save_dictionary_order(config, &dictionaries, cache_dir, anki_json_path)
        {
            refresh_dictionary_settings(window, Some(manager), config);
            window.set_settings_status(format!("Could not save dictionary order: {error}").into());
            return;
        }
        apply_dictionary_order(config, &mut dictionary.borrow_mut());
    }
    refresh_dictionary_settings(window, Some(manager), config);
    window.set_settings_status("Dictionary settings updated".into());
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
}


fn install_subtitle_selection(
    app: &MementoApp,
    subtitles: Rc<RefCell<SubtitleController>>,
    rows: Rc<VecModel<SubEntry>>,
) {
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
    let weak = app.as_weak();
    app.on_search_subtitles(move |query| {
        let Some(app) = weak.upgrade() else { return; };
        app.set_subtitle_search_query(query.clone());
        let matching = if query.trim().is_empty() { Vec::new() } else { subtitles.borrow().search(&query, true) };
        for index in 0..rows.row_count() {
            if let Some(mut row) = rows.row_data(index) {
                let is_match = matching.contains(&index);
                if row.search_match != is_match {
                    row.search_match = is_match;
                    rows.set_row_data(index, row);
                }
            }
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

fn keyboard_chord(
    key: &str,
    control: bool,
    shift: bool,
    alt: bool,
    meta: bool,
) -> String {
    let mut chord = String::new();
    if control { chord.push_str("Ctrl+"); }
    if alt { chord.push_str("Alt+"); }
    if shift { chord.push_str("Shift+"); }
    if meta { chord.push_str("Meta+"); }
    chord.push_str(&key.to_uppercase());
    chord
}


/// Opens native zenity file picker.
fn pick_file_with_zenity() -> Option<PathBuf> {
    let output = Command::new("zenity")
        .args([
            "--file-selection",
            "--title=Select Media or Subtitle File",
            "--file-filter=Media Files | *.mkv *.mp4 *.webm *.avi *.mp3 *.flac *.wav *.ogg *.m4a *.ts *.mov *.ogv *.srt *.ass *.vtt",
            "--file-filter=All Files | *",
        ])
        .output()
        .ok()?;

    if output.status.success() {
        let path_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !path_str.is_empty() {
            return Some(PathBuf::from(path_str));
        }
    }
    None
}
fn pick_dictionary_archive() -> Option<PathBuf> {
    let output = Command::new("zenity")
        .args([
            "--file-selection",
            "--title=Select Yomitan Dictionary Archive",
            "--file-filter=ZIP Archives | *.zip",
            "--file-filter=All Files | *",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!path.is_empty()).then(|| PathBuf::from(path))
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

fn refresh_recent_files(app: &MementoApp, config: &ConfigController) {
    let files: Vec<slint::SharedString> = config.recent_files().into_iter().map(Into::into).collect();
    app.set_recent_files(Rc::new(VecModel::from(files)).into());
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
    app.set_furigana_visible(setting_enabled(
        config, raw::search::GROUP, raw::search::SUBTITLE_FURIGANA,
        raw::search::SUBTITLE_FURIGANA_DEFAULT,
    ));
    app.set_furigana_enabled(true);
    app.set_osc_duration_ms(setting_number(
        config, raw::behavior::GROUP, raw::behavior::OSC_DURATION,
        raw::behavior::OSC_DURATION_DEFAULT as f64,
    ).round().clamp(50.0, 9999.0) as i32);
    app.set_osc_min_move(setting_number(
        config, raw::behavior::GROUP, raw::behavior::OSC_MIN_MOVE,
        raw::behavior::OSC_MIN_MOVE_DEFAULT as f64,
    ).round().max(0.0) as i32);
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
}

fn refresh_category_settings(window: &SettingsWindow, config: &ConfigController, category: i32) {
    window.set_settings_groups(Rc::new(VecModel::from(config.category_settings_groups(category))).into());
    window.set_current_anki_profile(config.current_anki_profile().into());
    window.set_current_keybind_profile(config.current_keybind_profile().into());
}

fn refresh_settings(window: &SettingsWindow, config: &ConfigController) {
    refresh_category_settings(window, config, window.get_active_category());
}

fn report_settings(
    window: &slint::Weak<SettingsWindow>,
    config: &ConfigController,
    result: Result<(), String>,
) {
    if let Some(window) = window.upgrade() {
        match result {
            Ok(()) => {
                refresh_settings(&window, config);
                window.set_settings_status("Settings updated".into());
            }
            Err(error) => window.set_settings_status(error.into()),
        }
    }
}

struct SettingsSession {
    window: SettingsWindow,
    _field_timer: slint::Timer,
}

fn clear_asr_for_settings(
    app: &MementoApp,
    asr: &Rc<RefCell<controller::asr::AsrController>>,
    subtitles: &Rc<RefCell<SubtitleController>>,
    subtitle_model: &Rc<VecModel<SubEntry>>,
    restart_pending: &Rc<Cell<bool>>,
    mpv: Option<&Rc<RefCell<Option<MpvInstance>>>>,
    coordinator: Option<&Rc<RefCell<Option<LivePlayerCoordinator>>>>,
) {
    let selected = app.get_asr_track_selected() || app.get_asr_running();
    let restart = selected && app.get_asr_enabled();
    asr.borrow_mut().cancel();
    if let Some(coord_rc) = coordinator {
        if let Ok(mut coord_guard) = coord_rc.try_borrow_mut() {
            if let Some(coord) = coord_guard.as_mut() {
                let player_guard = mpv.and_then(|p| p.try_borrow().ok());
                let player_ref = player_guard.as_deref().and_then(Option::as_ref);
                coord.remove_asr_track(player_ref);
            }
        }
    }
    let mut subtitles = subtitles.borrow_mut();
    subtitles.clear_asr_track();
    refresh_subtitle_list(app, subtitle_model, &subtitles);
    app.set_asr_running(false);
    app.set_asr_track_selected(restart);
    restart_pending.set(restart);
    if selected {
        clear_primary_subtitle(app);
    }
}

#[allow(clippy::too_many_arguments)]
fn create_settings_session(
    app: &MementoApp,
    config_ctrl: Rc<ConfigController>,
    dict_ctrl: Rc<RefCell<DictController>>,
    dictionary_manager: Option<Rc<DictionaryManager>>,
    dictionary_database_path: PathBuf,
    cache_dir: PathBuf,
    anki_json_path: PathBuf,
    ocr_available: bool,
    asr_session: Rc<RefCell<controller::asr::AsrController>>,
    subtitles: Rc<RefCell<SubtitleController>>,
    subtitle_model: Rc<VecModel<SubEntry>>,
    asr_restart_pending: Rc<Cell<bool>>,
    mpv: Rc<RefCell<Option<MpvInstance>>>,
    coordinator: Rc<RefCell<Option<LivePlayerCoordinator>>>,
) -> Result<SettingsSession, slint::PlatformError> {
    let settings = SettingsWindow::new()?;
    settings.set_ocr_built(cfg!(feature = "ocr"));
    settings.set_ocr_available(ocr_available);
    settings.set_asr_available(controller::asr::AVAILABLE);
    refresh_settings(&settings, &config_ctrl);
    refresh_dictionary_settings(&settings, dictionary_manager.as_deref(), &config_ctrl);

    // A native Help window on Wayland becomes visible when it is created.
    // Construct it only when Help is requested.
    let help_window = Rc::new(RefCell::new(None::<AnkiHelpWindow>));
    let help_on_request = help_window.clone();
    settings.on_anki_help_requested(move || {
        if help_on_request.borrow().is_none() {
            match AnkiHelpWindow::new() {
                Ok(window) => {
                    anki_help::populate(&window);
                    *help_on_request.borrow_mut() = Some(window);
                }
                Err(error) => {
                    eprintln!("Could not create Anki help: {error}");
                    return;
                }
            }
        }
        if let Some(window) = help_on_request.borrow().as_ref() {
            if let Err(error) = window.show() {
                eprintln!("Could not open Anki help: {error}");
            }
        }
    });

    let category_window = settings.as_weak();
    let category_config = config_ctrl.clone();
    let category_manager = dictionary_manager.clone();
    settings.on_category_changed(move |category| {
        if let Some(window) = category_window.upgrade() {
            refresh_category_settings(&window, &category_config, category);
            if category == 4 {
                refresh_dictionary_settings(&window, category_manager.as_deref(), &category_config);
            }
            window.window().request_redraw();
        }
    });

    let close_settings_weak = settings.as_weak();
    settings.window().on_close_requested(move || {
        if let Some(settings) = close_settings_weak.upgrade() {
            let _ = settings.hide();
        }
        slint::CloseRequestResponse::HideWindow
    });
    let close_settings_btn = settings.as_weak();
    settings.on_close(move || {
        if let Some(settings) = close_settings_btn.upgrade() {
            let _ = settings.hide();
        }
    });
    let edited_window = settings.as_weak();
    let edited_config = config_ctrl.clone();
    let edited_manager = dictionary_manager.clone();
    settings.on_setting_edited(move |key, value| {
        if let Some(window) = edited_window.upgrade() {
            match edited_config.edit_setting(&key, &value) {
                Ok(()) => {
                    if matches!(value.as_str(), "true" | "false")
                        || matches!(key.as_str(), "anki/profile" | "keybind/current")
                        || key.ends_with("/deck") || key.ends_with("/model")
                    {
                        refresh_settings(&window, &edited_config);
                    }
                    if key == "dictionary/order" {
                        refresh_dictionary_settings(
                            &window,
                            edited_manager.as_deref(),
                            &edited_config,
                        );
                    }
                    window.set_settings_status("Unsaved changes — click Apply".into());
                    if matches!(key.as_str(), "anki/profile/term/model" | "anki/profile/kanji/model")
                        && (0..window.get_available_anki_models().row_count())
                            .filter_map(|index| window.get_available_anki_models().row_data(index))
                            .any(|model| model == value)
                    {
                        window.invoke_load_anki_fields(if key.contains("/term/") { "term" } else { "kanji" }.into());
                    }
                }
                Err(error) => window.set_settings_status(error.into()),
            }
        }
    });
    let apply_window = settings.as_weak();
    let apply_config = config_ctrl.clone();
    let apply_cache = cache_dir.clone();
    let apply_anki = anki_json_path.clone();
    let apply_dict = dict_ctrl.clone();
    let apply_app = app.as_weak();
    let apply_manager = dictionary_manager.clone();
    let apply_asr = asr_session.clone();
    let apply_subtitles = subtitles.clone();
    let apply_subtitle_model = subtitle_model.clone();
    let apply_restart = asr_restart_pending.clone();
    let apply_mpv = mpv.clone();
    let apply_coord = coordinator.clone();
    settings.on_apply_settings(move |category| {
        let result =
            apply_config.apply_settings(category, &apply_cache, "memento.conf", &apply_anki);
        if result.is_ok() {
            if let Some(app) = apply_app.upgrade() {
                refresh_player_preferences(&app, &apply_config);
                if category == 4 {
                    apply_dictionary_order(&apply_config, &mut apply_dict.borrow_mut());
                    if let Some(window) = apply_window.upgrade() {
                        refresh_dictionary_settings(
                            &window,
                            apply_manager.as_deref(),
                            &apply_config,
                        );
                    }
                }
                app.set_ocr_enabled(
                    ocr_available && setting_enabled(
                        &apply_config, raw::ocr::GROUP, raw::ocr::ENABLED,
                        raw::ocr::ENABLED_DEFAULT,
                    ),
                );
                app.set_asr_enabled(controller::asr::AVAILABLE &&
                    setting_enabled(
                        &apply_config, raw::asr::GROUP, raw::asr::ENABLED,
                        raw::asr::ENABLED_DEFAULT,
                    ),
                );
                if category == 8 {
                    let config_dir = memento_xdg_dir("XDG_CONFIG_HOME", ".config")
                        .unwrap_or_else(|| PathBuf::from("."));
                    let unchanged = controller::asr::AsrSettings::from_config(config_dir, &apply_config.store())
                        .is_ok_and(|settings| apply_asr.borrow().matches_settings(&settings));
                    if !unchanged {
                        clear_asr_for_settings(&app, &apply_asr, &apply_subtitles, &apply_subtitle_model, &apply_restart, Some(&apply_mpv), Some(&apply_coord));
                    }
                }
            }
        }
        report_settings(&apply_window, &apply_config, result);
    });
    let reset_window = settings.as_weak();
    let reset_config = config_ctrl.clone();
    let reset_cache = cache_dir.clone();
    let reset_anki = anki_json_path.clone();
    let reset_dict = dict_ctrl.clone();
    let reset_manager = dictionary_manager.clone();
    let reset_app = app.as_weak();
    let reset_asr = asr_session.clone();
    let reset_subtitles = subtitles.clone();
    let reset_subtitle_model = subtitle_model.clone();
    let reset_restart = asr_restart_pending.clone();
    let reset_mpv = mpv.clone();
    let reset_coord = coordinator.clone();
    settings.on_reset_settings(move |category| {
        let result =
            reset_config.reset_settings(category, &reset_cache, "memento.conf", &reset_anki);
        if result.is_ok() {
            if let Some(app) = reset_app.upgrade() {
                refresh_player_preferences(&app, &reset_config);
                app.set_ocr_enabled(
                    ocr_available && setting_enabled(
                        &reset_config, raw::ocr::GROUP, raw::ocr::ENABLED,
                        raw::ocr::ENABLED_DEFAULT,
                    ),
                );
                app.set_asr_enabled(controller::asr::AVAILABLE &&
                    setting_enabled(
                        &reset_config, raw::asr::GROUP, raw::asr::ENABLED,
                        raw::asr::ENABLED_DEFAULT,
                    ),
                );
                if category == 8 {
                    clear_asr_for_settings(&app, &reset_asr, &reset_subtitles, &reset_subtitle_model, &reset_restart, Some(&reset_mpv), Some(&reset_coord));
                }
            }
            if category == 4 {
                apply_dictionary_order(&reset_config, &mut reset_dict.borrow_mut());
                if let Some(window) = reset_window.upgrade() {
                    refresh_dictionary_settings(
                        &window,
                        reset_manager.as_deref(),
                        &reset_config,
                    );
                }
            }
        }
        report_settings(&reset_window, &reset_config, result);
    });
    let defaults_window = settings.as_weak();
    let defaults_config = config_ctrl.clone();
    let defaults_app = app.as_weak();
    let defaults_asr = asr_session.clone();
    let defaults_subtitles = subtitles.clone();
    let defaults_subtitle_model = subtitle_model.clone();
    let defaults_restart = asr_restart_pending.clone();
    let defaults_mpv = mpv.clone();
    let defaults_coord = coordinator.clone();
    settings.on_defaults_settings(move |category| {
        let result = defaults_config.defaults_settings(category);
        if result.is_ok() && category == 8 {
            if let Some(app) = defaults_app.upgrade() {
                app.set_asr_enabled(controller::asr::AVAILABLE && setting_enabled(&defaults_config, raw::asr::GROUP, raw::asr::ENABLED, raw::asr::ENABLED_DEFAULT));
                clear_asr_for_settings(&app, &defaults_asr, &defaults_subtitles, &defaults_subtitle_model, &defaults_restart, Some(&defaults_mpv), Some(&defaults_coord));
            }
        }
        report_settings(
            &defaults_window, &defaults_config,
            result,
        );
    });
    let add_window = settings.as_weak();
    let add_config = config_ctrl.clone();
    settings.on_add_entry(move |category, name| {
        report_settings(&add_window, &add_config, add_config.add_entry(category, &name));
    });
    let remove_window = settings.as_weak();
    let remove_config = config_ctrl.clone();
    settings.on_remove_entry(move |category, name| {
        report_settings(&remove_window, &remove_config, remove_config.remove_entry(category, &name));
    });
    let move_window = settings.as_weak();
    let move_config = config_ctrl.clone();
    settings.on_move_entry(move |category, name, direction| {
        report_settings(
            &move_window, &move_config,
            move_config.move_entry(category, &name, direction),
        );
    });
    let add_dictionary_window = settings.as_weak();
    let add_dictionary_manager = dictionary_manager.clone();
    let add_dictionary_controller = dict_ctrl.clone();
    let add_dictionary_path = dictionary_database_path.clone();
    let add_dictionary_config = config_ctrl.clone();
    let add_dictionary_cache = cache_dir.clone();
    let add_dictionary_anki = anki_json_path.clone();
    settings.on_add_dictionary_archive(move || {
        let Some(window) = add_dictionary_window.upgrade() else { return };
        let Some(archive) = pick_dictionary_archive() else { return };
        let Some(manager) = add_dictionary_manager.as_deref() else {
            window.set_settings_status("Dictionary manager is unavailable".into());
            return;
        };
        let result = mutate_dictionary_database(
            &add_dictionary_controller,
            &add_dictionary_path,
            &add_dictionary_config,
            || manager.add_dictionary(&archive),
        );
        match result {
            Ok(dictionaries) => finish_dictionary_change(
                &window,
                manager,
                &add_dictionary_config,
                &add_dictionary_controller,
                &add_dictionary_cache,
                &add_dictionary_anki,
                dictionaries,
                true,
            ),
            Err(error) => window.set_settings_status(format!("Could not add dictionary: {error}").into()),
        }
    });
    let toggle_dictionary_window = settings.as_weak();
    let toggle_dictionary_manager = dictionary_manager.clone();
    let toggle_dictionary_controller = dict_ctrl.clone();
    let toggle_dictionary_path = dictionary_database_path.clone();
    let toggle_dictionary_config = config_ctrl.clone();
    settings.on_dictionary_enabled_changed(move |id, enabled| {
        let Some(window) = toggle_dictionary_window.upgrade() else { return };
        let Some(manager) = toggle_dictionary_manager.as_deref() else {
            window.set_settings_status("Dictionary manager is unavailable".into());
            return;
        };
        let result = mutate_dictionary_database(
            &toggle_dictionary_controller,
            &toggle_dictionary_path,
            &toggle_dictionary_config,
            || {
                if enabled {
                    manager.enable_dictionary(i64::from(id))
                } else {
                    manager.disable_dictionary(i64::from(id))
                }
            },
        );
        match result {
            Ok(_) => {
                refresh_dictionary_settings(
                    &window,
                    Some(manager),
                    &toggle_dictionary_config,
                );
                window.set_settings_status("Dictionary enabled state updated".into());
            }
            Err(error) => window.set_settings_status(
                format!("Could not change dictionary state: {error}").into(),
            ),
        }
    });
    let delete_dictionary_window = settings.as_weak();
    let delete_dictionary_manager = dictionary_manager.clone();
    let delete_dictionary_controller = dict_ctrl.clone();
    let delete_dictionary_path = dictionary_database_path.clone();
    let delete_dictionary_config = config_ctrl.clone();
    let delete_dictionary_cache = cache_dir.clone();
    let delete_dictionary_anki = anki_json_path.clone();
    settings.on_delete_dictionary(move |id| {
        let Some(window) = delete_dictionary_window.upgrade() else { return };
        let Some(manager) = delete_dictionary_manager.as_deref() else {
            window.set_settings_status("Dictionary manager is unavailable".into());
            return;
        };
        let result = mutate_dictionary_database(
            &delete_dictionary_controller,
            &delete_dictionary_path,
            &delete_dictionary_config,
            || manager.delete_dictionary(i64::from(id)),
        );
        match result {
            Ok(dictionaries) => finish_dictionary_change(
                &window,
                manager,
                &delete_dictionary_config,
                &delete_dictionary_controller,
                &delete_dictionary_cache,
                &delete_dictionary_anki,
                dictionaries,
                true,
            ),
            Err(error) => window.set_settings_status(format!("Could not delete dictionary: {error}").into()),
        }
    });
    let move_dictionary_up_window = settings.as_weak();
    let move_dictionary_up_manager = dictionary_manager.clone();
    let move_dictionary_up_controller = dict_ctrl.clone();
    let move_dictionary_up_config = config_ctrl.clone();
    let move_dictionary_up_cache = cache_dir.clone();
    let move_dictionary_up_anki = anki_json_path.clone();
    settings.on_move_dictionary_up(move |id| {
        let Some(window) = move_dictionary_up_window.upgrade() else { return };
        let Some(manager) = move_dictionary_up_manager.as_deref() else {
            window.set_settings_status("Dictionary manager is unavailable".into());
            return;
        };
        match move_dictionary_order(
            manager,
            &move_dictionary_up_config,
            &move_dictionary_up_controller,
            &move_dictionary_up_cache,
            &move_dictionary_up_anki,
            id,
            -1,
        ) {
            Ok(()) => {
                refresh_dictionary_settings(
                    &window,
                    Some(manager),
                    &move_dictionary_up_config,
                );
                window.set_settings_status("Dictionary order updated".into());
            }
            Err(error) => window.set_settings_status(format!("Could not move dictionary: {error}").into()),
        }
    });
    let move_dictionary_down_window = settings.as_weak();
    let move_dictionary_down_manager = dictionary_manager.clone();
    let move_dictionary_down_controller = dict_ctrl.clone();
    let move_dictionary_down_config = config_ctrl.clone();
    let move_dictionary_down_cache = cache_dir.clone();
    let move_dictionary_down_anki = anki_json_path.clone();
    settings.on_move_dictionary_down(move |id| {
        let Some(window) = move_dictionary_down_window.upgrade() else { return };
        let Some(manager) = move_dictionary_down_manager.as_deref() else {
            window.set_settings_status("Dictionary manager is unavailable".into());
            return;
        };
        match move_dictionary_order(
            manager,
            &move_dictionary_down_config,
            &move_dictionary_down_controller,
            &move_dictionary_down_cache,
            &move_dictionary_down_anki,
            id,
            1,
        ) {
            Ok(()) => {
                refresh_dictionary_settings(
                    &window,
                    Some(manager),
                    &move_dictionary_down_config,
                );
                window.set_settings_status("Dictionary order updated".into());
            }
            Err(error) => window.set_settings_status(format!("Could not move dictionary: {error}").into()),
        }
    });
    let test_window = settings.as_weak();
    let test_config = config_ctrl.clone();
    settings.on_test_anki_connection(move || {
        let endpoint = test_config.anki_endpoint();
        let response_window = test_window.clone();
        std::thread::spawn(move || {
            let controller = AnkiController::new(endpoint);
            let result = controller.test_connection().and_then(|version| {
                let decks = controller.client().deck_names().map_err(|error| error.to_string())?;
                let models = controller.client().model_names().map_err(|error| error.to_string())?;
                Ok((version, decks, models))
            });
            let _ = response_window.upgrade_in_event_loop(move |window| {
                let (ok, text) = match result {
                    Ok((version, decks, models)) => {
                        window.set_available_anki_decks(Rc::new(VecModel::from(decks.into_iter().map(Into::into).collect::<Vec<_>>())).into());
                        window.set_available_anki_models(Rc::new(VecModel::from(models.into_iter().map(Into::into).collect::<Vec<_>>())).into());
                        (true, format!("AnkiConnect is running (API version {version}). Decks and models are ready to select."))
                    }
                    Err(error) => (false, format!("Could not reach AnkiConnect: {error}\nMake sure Anki is open and the AnkiConnect add-on is installed.")),
                };
                window.set_settings_status(if ok { "Anki connection OK".into() } else { "Anki connection failed".into() });
                window.set_connection_popup_ok(ok);
                window.set_connection_popup_text(text.into());
                window.set_connection_popup_open(true);
            });
        });
    });
    let field_window = settings.as_weak();
    let field_config = config_ctrl.clone();
    let (field_tx, field_rx) = std::sync::mpsc::channel::<(String, String, String, Result<Vec<String>, String>)>();
    let field_timer = slint::Timer::default();
    let field_timer_window = settings.as_weak();
    let field_timer_config = config_ctrl.clone();
    field_timer.start(slint::TimerMode::Repeated, std::time::Duration::from_millis(100), move || {
        while let Ok((section, profile_name, model, result)) = field_rx.try_recv() {
            let Some(window) = field_timer_window.upgrade() else { continue };
            if field_timer_config.current_anki_profile() != profile_name
                || field_timer_config.anki_note_model(&section).as_deref() != Some(model.as_str()) {
                continue;
            }
            match result {
                Ok(fields) if !fields.is_empty() => {
                    match field_timer_config.set_anki_model_fields(&section, &fields) {
                        Ok(()) => {
                            refresh_category_settings(&window, &field_timer_config, 0);
                            window.set_settings_status(format!("Loaded {} model fields — click Apply to save", fields.len()).into());
                        }
                        Err(error) => window.set_settings_status(format!("Could not update Anki fields: {error}").into()),
                    }
                }
                Ok(_) => window.set_settings_status("The selected Anki model has no fields".into()),
                Err(error) => window.set_settings_status(format!("Could not load Anki fields: {error}").into()),
            }
        }
    });
    settings.on_load_anki_fields(move |section| {
        let Some(window) = field_window.upgrade() else { return };
        let section = section.to_string();
        let Some(model) = field_config.anki_note_model(&section).filter(|model| !model.trim().is_empty()) else {
            window.set_settings_status("Choose an Anki note model first".into());
            return;
        };
        window.set_settings_status(format!("Loading {model} fields from Anki…").into());
        let endpoint = field_config.anki_endpoint();
        let profile_name = field_config.current_anki_profile();
        let response_sender = field_tx.clone();
        std::thread::spawn(move || {
            let result = AnkiController::new(endpoint).client().field_names(&model)
                .map_err(|error| error.to_string());
            let _ = response_sender.send((section, profile_name, model, result));
        });
    });


    Ok(SettingsSession { window: settings, _field_timer: field_timer })
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

    // Subtitle model
    let sub_model = Rc::new(slint::VecModel::<SubEntry>::default());
    app.set_subtitle_entries(sub_model.clone().into());
    app.set_volume(100.0);
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
    let shortcut_app = app.as_weak();
    app.on_key_pressed(move |text, control, shift, alt, meta| {
        if key_closing.get() { return true; }
        let Some(app) = shortcut_app.upgrade() else { return false };
        // Any keyboard input dismisses a definition, even when the key is
        // handled by the player or a child widget.
        let had_dict_open = app.get_dict_open();
        app.invoke_dismiss_definition();
        let chord = keyboard_chord(&text, control, shift, alt, meta);
        let assigned = |binding: &slint::SharedString| {
            !binding.is_empty() && chord.eq_ignore_ascii_case(binding.as_str())
        };
        if assigned(&app.get_key_open_file()) {
            app.invoke_open_file();
        } else if assigned(&app.get_key_open_url()) {
            app.invoke_open_url();
        } else if assigned(&app.get_key_show_search()) {
            app.set_dict_search_open(!app.get_dict_search_open());
            app.set_sub_drawer_open(false);
        } else if assigned(&app.get_key_show_sub_list()) {
            app.set_sub_drawer_open(!app.get_sub_drawer_open());
            app.set_dict_search_open(false);
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
            if app.get_ocr_mode() {
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
                "\u{0009}" => Some("TAB"),
                "\u{007f}" => Some("DEL"),
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

    // The video surface is a Slint window, so mpv's fullscreen key cannot
    // fullscreen it. Keep the OSC timer in the UI layer as well.
    let osc_timer = Rc::new(slint::Timer::default());
    let osc_timer_app = app.as_weak();
    osc_timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(app.get_osc_duration_ms() as u64),
        move || {
            let Some(app) = osc_timer_app.upgrade() else { return };
            let fullscreen = app.window().is_fullscreen();
            if app.get_is_fullscreen() != fullscreen {
                app.set_is_fullscreen(fullscreen);
            }
            if app.get_duration() > 0.0 && !app.get_menu_open() && !app.get_dict_open() {
                app.set_osc_visible(false);
            }
        },
    );
    let pointer_app = app.as_weak();
    let pointer_osc_timer = osc_timer.clone();
    let pointer_mpv = mpv.clone();
    let pointer_coord = coordinator.clone();
    let pointer_render_size = current_render_size.clone();
    let last_pointer = Rc::new(Cell::new(None::<(f32, f32)>));
    app.on_pointer_moved(move |x, y| {
        let Some(app) = pointer_app.upgrade() else { return };
        if !app.get_dict_open() && !app.get_menu_open() && !app.get_ocr_mode() && !app.get_about_open() {
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
        app.set_osc_visible(true);
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
    app.on_osc_activity(move || {
        let Some(app) = activity_app.upgrade() else { return };
        app.set_osc_visible(true);
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

    let weak_app = app.as_weak();
    let open_mpv = mpv.clone();
    let open_coord = coordinator.clone();
    let open_sub_ctrl = sub_ctrl.clone();
    let open_sub_model = sub_model.clone();
    let open_current_media = current_media.clone();
    let open_config = config_ctrl.clone();
    let open_generation = media_generation.clone();
    let open_shared_gen = shared_media_generation.clone();
    let open_cache = config_dir.clone();

    app.on_open_file(move || {
        let Some(file_path) = pick_file_with_zenity() else { return };
        let file_str = file_path.to_string_lossy().into_owned();
        let Some(app) = weak_app.upgrade() else { return };
        let player = open_mpv.borrow();
        let Some(player) = player.as_ref() else {
            app.set_status_message("Playback backend unavailable".into());
            return;
        };
        if controller::player::is_subtitle_file(file_path.as_path()) {
            if let Err(error) = player.command(&["sub-add", &file_str]) {
                app.set_status_message(format!("Could not add subtitles: {error}").into());
            }
            return;
        }
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
                if let Err(error) =
                    record_recent_media(&app, &open_config, &open_cache, &file_str)
                {
                    app.set_status_message(format!("Cannot save recent media: {error}").into());
                }
            }
            Err(error) => app.set_status_message(format!("Cannot open media: {error}").into()),
        }
    });

    let recent_app = app.as_weak();
    let recent_mpv = mpv.clone();
    let recent_coord = coordinator.clone();
    let recent_subs = sub_ctrl.clone();
    let recent_model = sub_model.clone();
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
    let secondary_app = app.as_weak();
    app.on_select_secondary_subtitle_track(move |id| {
        if let Some(player) = &*secondary_mpv.borrow() {
            match player.set_secondary_sid(id.into()) {
                Ok(()) => {
                    secondary_subs.borrow_mut().set_secondary_sid(id.into());
                    if let Some(app) = secondary_app.upgrade() {
                        app.set_secondary_sid(id);
                    }
                }
                Err(error) => eprintln!("Cannot select secondary subtitle track {id}: {error}"),
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

    let vol_mpv = mpv.clone();
    app.on_set_volume(move |vol| {
        if let Some(mpv_inst) = &*vol_mpv.borrow() {
            let _ = mpv_inst.set_volume(vol as f64);
            if vol > 0.0 {
                let _ = mpv_inst.command_async(&["set", "mute", "no"]);
            }
        }
    });

    // ── Menu Bar & Shortcut Callbacks ───────────────────────────────────────
    let weak_mute = app.as_weak();
    let mute_mpv = mpv.clone();
    app.on_toggle_mute(move || {
        if let Some(mpv_inst) = &*mute_mpv.borrow() {
            let _ = mpv_inst.command_async(&["cycle", "mute"]);
        } else if let Some(app) = weak_mute.upgrade() {
            let vol = app.get_volume();
            app.set_volume(if vol > 0.0 { 0.0 } else { 100.0 });
        }
    });

    let weak_vup = app.as_weak();
    let vup_mpv = mpv.clone();
    app.on_volume_up(move || {
        if let Some(app) = weak_vup.upgrade() {
            let new_vol = (app.get_volume() + 5.0).min(100.0);
            app.set_volume(new_vol);
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
            let new_vol = (app.get_volume() - 5.0).max(0.0);
            app.set_volume(new_vol);
            if let Some(mpv_inst) = &*vdown_mpv.borrow() {
                let _ = mpv_inst.set_volume(new_vol as f64);
            }
        }
    });

    let weak_url = app.as_weak();
    let url_mpv = mpv.clone();
    let url_coord = coordinator.clone();
    let url_current = current_media.clone();
    let url_subs = sub_ctrl.clone();
    let url_generation = media_generation.clone();
    let url_shared_gen = shared_media_generation.clone();
    let url_model = sub_model.clone();
    app.on_open_url(move || {
        let Some(app) = weak_url.upgrade() else { return };
        let output = match Command::new("zenity")
            .args(["--entry", "--title=Open Stream URL", "--text=Enter video URL:"])
            .output()
        {
            Ok(output) => output,
            Err(error) => {
                app.set_status_message(format!("Cannot open URL dialog: {error}").into());
                return;
            }
        };
        if !output.status.success() { return }
        let url = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if url.is_empty() { return }
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
                }
                Err(error) => app.set_status_message(format!("Cannot open URL: {error}").into()),
            }
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
    let close_current = current_media.clone();
    let close_generation = media_generation.clone();
    let close_shared_gen = shared_media_generation.clone();
    let close_asr = asr_session.clone();
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
        close_asr.borrow_mut().cancel();
        let next_gen = close_generation.get().wrapping_add(1);
        close_generation.set(next_gen);
        close_shared_gen.store(next_gen, Ordering::Release);
        *close_current.borrow_mut() = None;
        close_sub_ctrl.borrow_mut().clear();
        close_sub_model.set_vec(Vec::new());
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
    let quit_app = app.as_weak();
    app.on_quit_app(move || {
        if quit_closing.replace(true) { return; }
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

    let add_sub_mpv = mpv.clone();
    let add_sub_app = app.as_weak();
    app.on_add_subtitle(move || {
        let Some(file_path) = pick_file_with_zenity() else { return };
        let Some(app) = add_sub_app.upgrade() else { return };
        let Some(player) = &*add_sub_mpv.borrow() else { return };
        if let Err(error) = player.command(&["sub-add", &file_path.to_string_lossy()]) {
            app.set_status_message(format!("Cannot add subtitles: {error}").into());
        }
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
                                app.set_dict_query(text.into());
                                app.set_dict_search_open(true);
                                app.set_sub_drawer_open(false);
                                app.invoke_search_dictionary(text.into());
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
        if let Some(app) = update_app.upgrade() {
            app.set_status_message("Checking for updates…".into());
        }
        let response_app = update_app.clone();
        std::thread::spawn(move || {
            let message = match latest_release() {
                Ok(tag) if tag == format!("v{}", env!("CARGO_PKG_VERSION")) =>
                    "You are on the latest version.".to_string(),
                Ok(tag) => format!("New Memento release available: {tag}"),
                Err(error) => format!("Could not check for updates: {error}"),
            };
            let _ = response_app.upgrade_in_event_loop(move |app| {
                app.set_status_message(message.into());
            });
        });
    });

    let config_dir_app = app.as_weak();
    app.on_open_config(move || {
        if let Some(path) = memento_xdg_dir("XDG_CONFIG_HOME", ".config") {
            if let Err(error) = std::fs::create_dir_all(&path)
                .and_then(|_| Command::new("xdg-open").arg(path).spawn().map(|_| ()))
            {
                if let Some(app) = config_dir_app.upgrade() {
                    app.set_status_message(format!("Could not open configuration directory: {error}").into());
                }
            }
        }
    });


    let weak_fs = app.as_weak();
    app.on_toggle_fullscreen(move || {
        if let Some(app) = weak_fs.upgrade() {
            let next = !app.window().is_fullscreen();
            app.window().set_fullscreen(next);
            app.set_is_fullscreen(next);
            app.set_osc_visible(true);
            app.invoke_osc_activity();
        }
    });

    install_subtitle_selection(&app, sub_ctrl.clone(), sub_model.clone());

    // ── Dictionary Lookup ──────────────────────────────────────────────────
    let popup_audio_cache: Arc<Mutex<PopupAudioState>> = Arc::new(Mutex::new(PopupAudioState::default()));
    let (lookup_tx, lookup_rx) = std::sync::mpsc::channel::<(u64, String, Vec<i64>, String, Vec<String>)>();
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
            if request.0 != worker_epoch.load(Ordering::Acquire) { continue; }
            let signature = controller::dict::db_file_fingerprint(&lookup_path);
            if signature != database_signature {
                dictionary = DictController::open(&lookup_path).unwrap_or_else(|error| {
                    eprintln!("Dictionary lookup unavailable: {error}");
                    DictController::new()
                });
                database_signature = signature;
            }
            dictionary.set_order(&request.2);
            let hits = dictionary.search(&request.1).to_vec();
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
            if request.0 != worker_epoch.load(Ordering::Acquire) { continue; }
            let generation = request.0;
            let query = request.1;
            let source_subtitle = request.3;
            let excluded = request.4;
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
        let generation = lookup_generation.fetch_add(1, Ordering::AcqRel) + 1;
        let _ = lookup_sender.send((generation, word.to_string(), order, app.get_active_sub_text().to_string(), lookup_config.anki_excluded_glossaries()));
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
            if let Err(error) = Command::new("xdg-open").arg(href.as_str()).spawn() {
                app.set_status_message(format!("Could not open dictionary link: {error}").into());
            }
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
        set_popup_audio_sources(&app, &search_audio_cache, "", "", search_dict_config.audio_source_entries());
        let hits = if query.trim().is_empty() {
            Vec::new()
        } else {
            let Ok(mut dictionary) = search_dict_ctrl.try_borrow_mut() else { return };
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

    let note_app = app.as_weak();
    let note_config = config_ctrl.clone();
    let note_audio_cache = popup_audio_cache.clone();
    let note_subtitles = sub_ctrl.clone();
    let note_mpv = mpv.clone();
    let note_database_path = dictionary_database_path.clone();
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
        let start = app.get_selected_token_index();
        if start >= 0 {
            let words = app.get_active_sub_words();
            let count = app.get_selected_token_count().max(1) as usize;
            let selection: String = ((start as usize)..(start as usize).saturating_add(count))
                .filter_map(|i| words.row_data(i))
                .map(|word| word.text.to_string())
                .collect();
            if !selection.is_empty() {
                context.selection = selection;
            }
        }
        if context.sentence.contains(&context.selection) {
            let (prefix, body, suffix) = controller::anki::split_cloze(&context.sentence, &context.selection);
            context.cloze_prefix = prefix.to_owned();
            context.cloze_body = body.to_owned();
            context.cloze_suffix = suffix.to_owned();
        }
        let selected_cues = note_subtitles.borrow().selected_cues()
            .iter().map(|cue| String::from_utf16_lossy(&cue.text)).collect::<Vec<_>>();
        if !selected_cues.is_empty() {
            context.context = selected_cues.join("\n");
        }
        context.secondary_subtitle = app.get_secondary_sub_text().to_string();
        context.title = app.get_media_title().to_string();
        context.dictionary_name = hit.dict_name.to_string();
        let selected: Vec<_> = (0..definitions.row_count())
            .filter_map(|row| definitions.row_data(row))
            .filter(|row| row.selected && row.expression == hit.expression && row.reading == hit.reading
                && row.is_kanji == hit.is_kanji)
            .collect();
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
        let (active_start, active_end, context_start, context_end) = {
            let subtitles = note_subtitles.borrow();
            let active = subtitles.active_cue();
            let start = active.map(|cue| cue.start).unwrap_or(0.0);
            let end = active.map(|cue| cue.end).unwrap_or(0.0);
            let (context_start, context_end) = subtitles.selected_time_range().unwrap_or((start, end));
            (start, end, context_start, context_end)
        };
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
        let unavailable = controller::anki::unavailable_profile_markers(&profile);
        app.set_status_message("Adding note to Anki…".into());
        let response_app = note_app.clone();
        let response_epoch = note_lookup_epoch.clone();
        let request_epoch = response_epoch.load(Ordering::Acquire);
        let metadata_path = note_database_path.clone();
        std::thread::spawn(move || {
            // Receive captures before any dictionary/audio/network work. The
            // commands were queued at the click and the UI keeps rendering.
            let captures = (|| -> Result<(), String> {
                media_input.screenshot = screenshot_request
                    .map(|request| request.and_then(|request| request.wait())).transpose()
                    .map_err(|error| format!("Could not capture {{screenshot}} for Anki: {error}"))?;
                media_input.screenshot_video = video_screenshot_request
                    .map(|request| request.and_then(|request| request.wait())).transpose()
                    .map_err(|error| format!("Could not capture {{screenshot-video}} for Anki: {error}"))?;
                Ok(())
            })();
            let controller = AnkiController::new(profile.endpoint());
            if let Ok(dictionary) = DictController::open(&metadata_path) {
                for definition in &mut context.glossary_definitions {
                    definition.dictionary_id = dictionary.dictionary_id_for_name(&definition.dictionary_name).unwrap_or(0);
                }
                let meta = dictionary.get_anki_term_metadata(&context.expression, &context.reading, &context.tags);
                context.pitch = meta.pitch;
                context.pitch_positions = meta.pitch_positions;
                context.pitch_categories = meta.pitch_categories;
                context.frequencies = meta.frequencies;
                context.frequency_harmonic_rank = meta.frequency_harmonic_rank;
                context.frequency_average_rank = meta.frequency_average_rank;
            }
            let result = captures.and_then(|()| controller::anki_media::prepare_note_media(&profile, &mut context, media_input))
                .and_then(|()| controller.add_note_from_profile(&profile, &context));
            let expire_app = response_app.clone();
            let _ = response_app.upgrade_in_event_loop(move |app| {
                if app.get_dict_open() && response_epoch.load(Ordering::Acquire) == request_epoch {
                    if let Ok(note_id) = &result {
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
        let Ok(note_id) = id.parse::<i64>() else { return };
        let endpoint = browse_config.anki_endpoint();
        let weak = browse_app.clone();
        std::thread::spawn(move || {
            let result = AnkiController::new(endpoint).client().open_browse(&format!("nid:{note_id}"));
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
        let verbose_video_timings = std::env::var_os("MEMENTO_VIDEO_STATS").is_some();
        let previous_furigana_visible = Cell::new(app.get_furigana_visible());
        let previous_subtitle_layout: Cell<Option<(i32, i32, u32, bool)>> = Cell::new(None);
        let last_video_report = Cell::new(std::time::Instant::now());
        let previous_drops = Cell::new((0i64, 0i64));
        let last_sync = Cell::new(std::time::Instant::now());

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

    let run_result = app.run();
    timer.stop();
    quit_timer.stop();
    MPV_EVENT_SYNC.with(|slot| *slot.borrow_mut() = None);
    EXTERNAL_SUBTITLE_SINK.with(|slot| *slot.borrow_mut() = None);
    drop(ext_sub_sender);
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

//! Main application entrypoint for pure-Rust Memento with Slint UI and live MPV backend.

use std::cell::{Cell, RefCell};
use std::process::Command;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use slint::{ComponentHandle, Model as _, Rgba8Pixel, SharedPixelBuffer, VecModel};

pub mod ui;
pub mod controller;

pub use ui::{
    DictResult, InstalledDictionaryRow, MementoApp, MementoPalette, PlayerTrack,
    SettingsWindow, SubEntry, SubtitleLine, SubtitleWord,
};
use controller::anki::AnkiController;
use controller::config::ConfigController;
use controller::dict::DictController;
use controller::subtitles::{MemorySubtitleCue, SubtitleController, SubtitleTrackInfo};
use controller::dictmanager::{DictionaryManager, InstalledDictionary};
use memento_config::Value;
use memento_player::MpvInstance;
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

/// Read a subtitle file supporting UTF-8 (with or without BOM), UTF-16 LE/BE, and lossy UTF-8.
fn read_subtitle_file(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|error| format!("Could not read {}: {error}", path.display()))?;
    if bytes.starts_with(b"\xef\xbb\xbf") {
        return Ok(String::from_utf8_lossy(&bytes[3..]).into_owned());
    }
    if bytes.starts_with(b"\xff\xfe") {
        let u16s: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
            .collect();
        return Ok(String::from_utf16_lossy(&u16s));
    }
    if bytes.starts_with(b"\xfe\xff") {
        let u16s: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|chunk| u16::from_be_bytes([chunk[0], chunk[1]]))
            .collect();
        return Ok(String::from_utf16_lossy(&u16s));
    }
    if bytes.len() >= 4 && bytes[1] == 0 && bytes[3] == 0 {
        let u16s: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
            .collect();
        return Ok(String::from_utf16_lossy(&u16s));
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
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

fn track_label(id: i64, language: &str, title: &str) -> String {
    let mut label = format!("Track {id}");
    if !language.is_empty() {
        label.push_str(&format!(" [{language}]"));
    }
    if !title.is_empty() {
        label.push_str(" - ");
        label.push_str(title);
    }
    label
}

fn load_media_into(
    app: &MementoApp,
    player: &MpvInstance,
    path: &str,
    media_generation: &Cell<u64>,
) -> Result<(), String> {
    player.load_file(path)?;
    media_generation.set(media_generation.get().wrapping_add(1));
    player.play()?;
    let title = Path::new(path).file_name().and_then(|name| name.to_str()).unwrap_or(path);
    app.set_media_title(title.into());
    app.set_active_sub_text("".into());
    app.set_secondary_sub_text("".into());
    app.set_active_sub_words(Rc::new(VecModel::from(Vec::new())).into());
    app.set_active_sub_lines(Rc::new(VecModel::from(Vec::new())).into());
    app.set_selected_token_index(-1);
    app.set_dict_open(false);
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

fn dictionary_result_rows(hits: &[controller::dict::SearchHit]) -> Vec<DictResult> {
    hits.iter().enumerate().map(|(index, hit)| DictResult {
        index: (index + 1) as i32,
        expression: hit.expression.clone().into(),
        reading: hit.reading.clone().into(),
        pos_tags: hit.tags.clone().into(),
        dict_name: hit.dict_name.clone().into(),
        glossary: hit.glossary.clone().into(),
        notes: hit.notes.clone().into(),
        example_ja: hit.example_ja.clone().into(),
        example_en: hit.example_en.clone().into(),
        selected: false,
        score: i32::try_from(hit.score).unwrap_or(if hit.score < 0 { i32::MIN } else { i32::MAX }),
        is_kanji: hit.is_kanji,
    }).collect()
}


fn refresh_subtitle_list(model: &VecModel<SubEntry>, subtitles: &SubtitleController) {
    let active = subtitles.active_indices(subtitles.primary_sid());
    let rows: Vec<SubEntry> = subtitles.primary_entries().iter().enumerate().map(|(index, cue)| SubEntry {
        id: (index + 1) as i32,
        start: cue.start as f32,
        end: cue.end as f32,
        text: String::from_utf16_lossy(&cue.text).into(),
        selected: subtitles.is_selected(index),
        is_active: active.contains(&index),
    }).collect();
    model.set_vec(rows);
}

fn refresh_active_rows(
    model: &VecModel<SubEntry>,
    previous: &[usize],
    active: &[usize],
) {
    for index in previous.iter().chain(active) {
        if let Some(mut row) = model.row_data(*index) {
            let now_active = active.contains(index);
            if row.is_active != now_active {
                row.is_active = now_active;
                model.set_row_data(*index, row);
            }
        }
    }
}

fn subtitle_words(text: &str) -> Vec<SubtitleWord> {
    let mut offset = 0;
    controller::subtitles::segment_subtitle_text(text)
        .into_iter()
        .enumerate()
        .map(|(index, token)| {
            let suffix = &text[offset..];
            let query = if token.chars().any(char::is_alphanumeric) {
                suffix.into()
            } else {
                slint::SharedString::default()
            };
            offset += token.len();
            SubtitleWord {
                text: token.into(),
                query,
                index: index as i32,
            }
        })
        .collect()
}

fn subtitle_lines(text: &str) -> Vec<SubtitleLine> {
    if text.is_empty() {
        return Vec::new();
    }
    let all_words = subtitle_words(text);
    let mut lines = Vec::new();
    let mut current_line: Vec<SubtitleWord> = Vec::new();
    let mut current_width = 0;

    for word in all_words {
        let word_text = word.text.as_str();
        if word_text.contains('\n') {
            if !current_line.is_empty() {
                lines.push(SubtitleLine {
                    words: Rc::new(VecModel::from(std::mem::take(&mut current_line))).into(),
                });
                current_width = 0;
            }
            continue;
        }
        let token_width: usize = word_text.chars().map(|c| {
            if matches!(c, '\u{3000}'..='\u{9FFF}' | '\u{F900}'..='\u{FAFF}' | '\u{FF01}'..='\u{FF60}') {
                2
            } else {
                1
            }
        }).sum();

        let is_closing_punct = matches!(word_text, "。" | "、" | "！" | "？" | "…" | "・" | "」" | "）" | "." | "," | "!" | "?" | ";" | ":");
        if !is_closing_punct && (
            (current_width + token_width > 42 && !current_line.is_empty())
            || (current_width >= 36 && word_text.trim().is_empty())
        ) {
            lines.push(SubtitleLine {
                words: Rc::new(VecModel::from(std::mem::take(&mut current_line))).into(),
            });
            current_width = 0;
            if word_text.trim().is_empty() {
                continue;
            }
        }
        current_width += token_width;
        current_line.push(word);
    }
    if !current_line.is_empty() {
        lines.push(SubtitleLine {
            words: Rc::new(VecModel::from(current_line)).into(),
        });
    }
    lines
}


fn sync_player_tracks(
    app: &MementoApp,
    player: &MpvInstance,
    subtitles: &mut SubtitleController,
    subtitle_rows: &VecModel<SubEntry>,
    previous_audio: &mut Vec<memento_player::AudioTrack>,
    previous_subtitles: &mut Vec<memento_player::SubtitleTrack>,
) {
    let audio = player.audio_tracks();
    if audio != *previous_audio {
        let rows: Vec<PlayerTrack> = audio.iter().filter_map(|track| {
            Some(PlayerTrack {
                id: i32::try_from(track.id).ok()?,
                label: track_label(track.id, &track.language, &track.title).into(),
                selected: track.selected,
            })
        }).collect();
        app.set_audio_tracks(Rc::new(VecModel::from(rows)).into());
        *previous_audio = audio;
    }
    app.set_audio_id(i32::try_from(player.selected_aid()).unwrap_or(0));

    let tracks = player.subtitle_tracks();
    if tracks != *previous_subtitles {
        let ui_tracks: Vec<PlayerTrack> = tracks.iter().filter_map(|track| {
            Some(PlayerTrack {
                id: i32::try_from(track.id).ok()?,
                label: track_label(track.id, &track.language, &track.title).into(),
                selected: track.selected,
            })
        }).collect();
        app.set_subtitle_tracks(Rc::new(VecModel::from(ui_tracks)).into());
        let info: Vec<SubtitleTrackInfo> = tracks.iter().map(|track| SubtitleTrackInfo {
            id: track.id,
            external: track.external,
            external_path: track.external_path.clone(),
        }).collect();
        for load in subtitles.sync_tracks(&info) {
            match parse_external_subtitles(&load.path) {
                Ok(cues) => {
                    subtitles.set_external_cues(
                        load.track_id, &load.path.to_string_lossy(), cues,
                    );
                }
                Err(error) => {
                    subtitles.set_external_cues(
                        load.track_id, &load.path.to_string_lossy(), Vec::new(),
                    );
                    app.set_status_message(error.into());
                }
            }
        }
        *previous_subtitles = tracks;
        refresh_subtitle_list(subtitle_rows, subtitles);
    }
    let primary = player.primary_sid();
    if subtitles.primary_sid() != primary {
        subtitles.set_primary_sid(primary);
        refresh_subtitle_list(subtitle_rows, subtitles);
    }
    let secondary = player.secondary_sid();
    if subtitles.secondary_sid() != secondary {
        subtitles.set_secondary_sid(secondary);
    }
    app.set_primary_sid(i32::try_from(primary).unwrap_or(0));
    app.set_secondary_sid(i32::try_from(secondary).unwrap_or(0));
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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = log::set_logger(&APP_LOGGER);
    log::set_max_level(log::LevelFilter::Warn);
    let app = MementoApp::new()?;

    let cache_dir = memento_xdg_dir("XDG_CACHE_HOME", ".cache")
        .ok_or("HOME or XDG_CACHE_HOME must be set to load Memento settings")?;
    let config_dir = memento_xdg_dir("XDG_CONFIG_HOME", ".config")
        .ok_or("HOME or XDG_CONFIG_HOME must be set to load Anki profiles")?;
    let data_dir = memento_xdg_dir("XDG_DATA_HOME", ".local/share")
        .ok_or("HOME or XDG_DATA_HOME must be set to load dictionaries")?;
    let anki_json_path = config_dir.join("anki_connect.json");
    let config = ConfigController::load(&cache_dir, "memento.conf")
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
    let args: Vec<String> = std::env::args().collect();
    let mpv = match MpvInstance::new_software(Some(&config_dir)) {
        Ok(instance) => Rc::new(RefCell::new(Some(instance))),
        Err(err) => {
            eprintln!("Could not initialize video playback: {err}");
            Rc::new(RefCell::new(None))
        }
    };

    // Subtitle model
    let sub_model = Rc::new(slint::VecModel::<SubEntry>::default());
    app.set_subtitle_entries(sub_model.clone().into());
    app.set_volume(100.0);
    app.set_current_time("00:00".into());
    app.set_duration_time("00:00".into());
    app.set_app_version(env!("CARGO_PKG_VERSION").into());
    refresh_player_preferences(&app, &config_ctrl);
    refresh_recent_files(&app, &config_ctrl);
    let settings = SettingsWindow::new()?;
    let ocr_controller = controller::ocr::OcrController::new();
    if let Err(error) = &ocr_controller {
        eprintln!("OCR unavailable: {error}");
    }
    let ocr_available = ocr_controller.is_ok();
    settings.set_ocr_available(ocr_available);
    settings.set_asr_available(true);
    let asr_session = Rc::new(RefCell::new(controller::asr::AsrController::new()));
    app.set_ocr_enabled(
        ocr_available && setting_enabled(
            &config_ctrl, raw::ocr::GROUP, raw::ocr::ENABLED,
            raw::ocr::ENABLED_DEFAULT,
        ),
    );
    app.set_asr_enabled(
        setting_enabled(
            &config_ctrl, raw::asr::GROUP, raw::asr::ENABLED,
            raw::asr::ENABLED_DEFAULT,
        ),
    );
    refresh_settings(&settings, &config_ctrl);
    refresh_dictionary_settings(&settings, dictionary_manager.as_deref(), &config_ctrl);

    let settings_open = settings.clone_strong();
    let config_open = config_ctrl.clone();
    let dict_mgr_open = dictionary_manager.clone();
    let app_weak = app.as_weak();
    app.on_open_options(move || {
        let cat = settings_open.get_active_category();
        refresh_category_settings(&settings_open, &config_open, cat);
        if cat == 4 {
            refresh_dictionary_settings(&settings_open, dict_mgr_open.as_deref(), &config_open);
        }
        if let Err(error) = settings_open.show() {
            eprintln!("Could not open preferences: {error}");
        }
        settings_open.window().request_redraw();
        if let Some(app) = app_weak.upgrade() {
            app.window().request_redraw();
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

    let selected_profile_app = app.as_weak();
    let selected_profile_config = config_ctrl.clone();
    let selected_profile_path = anki_json_path.clone();
    let selected_profile_cache = cache_dir.clone();
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
                app.set_asr_enabled(
                    setting_enabled(
                        &apply_config, raw::asr::GROUP, raw::asr::ENABLED,
                        raw::asr::ENABLED_DEFAULT,
                    ),
                );
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
                app.set_asr_enabled(
                    setting_enabled(
                        &reset_config, raw::asr::GROUP, raw::asr::ENABLED,
                        raw::asr::ENABLED_DEFAULT,
                    ),
                );
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
    settings.on_defaults_settings(move |category| {
        report_settings(
            &defaults_window, &defaults_config,
            defaults_config.defaults_settings(category),
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
            let result = AnkiController::new(endpoint).test_connection();
            let _ = response_window.upgrade_in_event_loop(move |window| {
                window.set_settings_status(match result {
                    Ok(version) => format!("AnkiConnect version {version}").into(),
                    Err(error) => format!("AnkiConnect: {error}").into(),
                });
            });
        });
    });


    let current_media: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let media_generation = Rc::new(Cell::new(0u64));
    // Timestamp formatting callback
    app.on_format_timestamp(|sec| format_time(sec as f64).into());

    let hover_cursor = Rc::new(RefCell::new(None::<(String, i32)>));
    let key_hover_cursor = hover_cursor.clone();
    let key_search_config = config_ctrl.clone();
    let key_mpv = mpv.clone();
    let shortcut_app = app.as_weak();
    app.on_key_pressed(move |text, control, shift, alt, meta| {
        let Some(app) = shortcut_app.upgrade() else { return false };
        let chord = keyboard_chord(&text, control, shift, alt, meta);
        let assigned = |binding: &slint::SharedString| {
            !binding.is_empty() && chord.eq_ignore_ascii_case(binding.as_str())
        };
        if matches!(
            key_search_config.effective_setting(raw::search::GROUP, raw::search::METHOD),
            Some(Value::Int(1)),
        ) {
            let modifier = match key_search_config
                .effective_setting(raw::search::GROUP, raw::search::MODIFIER)
            {
                Some(Value::Int(value)) => i32::try_from(value)
                    .unwrap_or(raw::search::MODIFIER_DEFAULT as i32),
                _ => raw::search::MODIFIER_DEFAULT as i32,
            };
            let held = match modifier {
                0 => alt,
                1 => control,
                2 => shift,
                3 => meta,
                _ => false,
            };
            if held {
                let target = key_hover_cursor.borrow().clone();
                if let Some((word, index)) = target {
                    app.invoke_hover_word(word.into(), index, control, shift, alt, meta);
                }
            }
        }
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
        } else if assigned(&app.get_key_sub_increase_size()) {
            app.invoke_adjust_subtitle_scale(0.005);
        } else if assigned(&app.get_key_sub_decrease_size()) {
            app.invoke_adjust_subtitle_scale(-0.005);
        } else if assigned(&app.get_key_sub_move_up()) {
            app.invoke_adjust_subtitle_offset(0.005);
        } else if assigned(&app.get_key_sub_move_down()) {
            app.invoke_adjust_subtitle_offset(-0.005);
        } else if chord.eq_ignore_ascii_case("ctrl+,") || (control && (text == "," || text == "<")) {
            app.invoke_open_options();
        } else if text == " " && !(control || shift || alt || meta) {
            app.invoke_toggle_play();
        } else if text == "\u{001b}" || text == "Escape" {
            if app.get_ocr_mode() {
                app.set_ocr_mode(false);
            } else if app.get_dict_open() {
                app.set_dict_open(false);
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
                let _ = slint::quit_event_loop();
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

    let initial_file = args.iter().skip(1).find(|a| !a.starts_with('-')).cloned();

    if let Some(file_str) = initial_file {
        if let Some(player) = &*mpv.borrow() {
            let generation = media_generation.clone();
            match load_media_into(&app, player, &file_str, &generation) {
                Ok(()) => {
                    *current_media.borrow_mut() = Some(file_str.clone());
                    if Path::new(&file_str).is_file() {
                        if let Err(error) =
                            record_recent_media(&app, &config_ctrl, &cache_dir, &file_str)
                        {
                            eprintln!("Cannot save recent media: {error}");
                        }
                    }
                }
                Err(error) => eprintln!("Cannot open {}: {error}", file_str),
            }
        }
    }

    let weak_app = app.as_weak();
    let open_mpv = mpv.clone();
    let open_sub_ctrl = sub_ctrl.clone();
    let open_sub_model = sub_model.clone();
    let open_current_media = current_media.clone();
    let open_config = config_ctrl.clone();
    let open_generation = media_generation.clone();
    let open_cache = cache_dir.clone();

    app.on_open_file(move || {
        let Some(file_path) = pick_file_with_zenity() else { return };
        let file_str = file_path.to_string_lossy().into_owned();
        let Some(app) = weak_app.upgrade() else { return };
        let player = open_mpv.borrow();
        let Some(player) = player.as_ref() else {
            app.set_status_message("Playback backend unavailable".into());
            return;
        };
        let is_subtitle = file_path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                matches!(extension.to_ascii_lowercase().as_str(), "srt" | "ass" | "ssa" | "vtt")
            });
        if is_subtitle {
            if let Err(error) = player.command(&["sub-add", &file_str]) {
                app.set_status_message(format!("Could not add subtitles: {error}").into());
            }
            return;
        }
        match load_media_into(&app, player, &file_str, &open_generation) {
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
    let recent_subs = sub_ctrl.clone();
    let recent_model = sub_model.clone();
    let recent_current = current_media.clone();
    let recent_generation = media_generation.clone();
    let recent_config = config_ctrl.clone();
    let recent_cache = cache_dir.clone();
    app.on_open_recent(move |path| {
        let Some(app) = recent_app.upgrade() else { return };
        let player = recent_mpv.borrow();
        let Some(player) = player.as_ref() else { return };
        match load_media_into(&app, player, &path, &recent_generation) {
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
    let clear_cache = cache_dir.clone();
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
    app.on_toggle_play(move || {
        if let Some(mpv_inst) = &*play_mpv.borrow() {
            let _ = mpv_inst.toggle_pause();
        }
    });

    let seek_mpv = mpv.clone();
    app.on_seek_to(move |sec| {
        if let Some(mpv_inst) = &*seek_mpv.borrow() {
            let _ = mpv_inst.seek(sec as f64);
        }
    });

    let rel_seek_mpv = mpv.clone();
    app.on_seek_relative(move |delta| {
        if let Some(mpv_inst) = &*rel_seek_mpv.borrow() {
            let _ = mpv_inst.seek_relative(delta as f64);
        }
    });

    let sub_seek_mpv = mpv.clone();
    app.on_sub_seek(move |direction| {
        if let Some(mpv_inst) = &*sub_seek_mpv.borrow() {
            let _ = mpv_inst.subtitle_seek(direction, true);
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
    let primary_subs = sub_ctrl.clone();
    let primary_app = app.as_weak();
    let primary_asr = asr_session.clone();
    app.on_select_subtitle_track(move |id| {
        if let Some(app) = primary_app.upgrade() {
            if app.get_asr_running() {
                primary_asr.borrow_mut().cancel();
                app.set_asr_running(false);
            }
        }
        if let Some(player) = &*primary_mpv.borrow() {
            match player.set_sid(id.into()) {
                Ok(()) => {
                    primary_subs.borrow_mut().set_primary_sid(id.into());
                    if let Some(app) = primary_app.upgrade() {
                        app.set_primary_sid(id);
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
    let select_asr_config = config_ctrl.clone();
    let select_asr_session = asr_session.clone();
    let select_asr_sub_ctrl = sub_ctrl.clone();
    app.on_select_asr(move || {
        let Some(app) = select_asr_app.upgrade() else { return };
        let mut asr = select_asr_session.borrow_mut();
        if app.get_asr_running() {
            asr.cancel();
            app.set_asr_running(false);
            app.set_status_message("ASR subtitles stopped".into());
            return;
        }
        if app.get_duration() <= 0.0 {
            app.set_status_message("Open a video before starting ASR subtitles".into());
            return;
        }
        let Some(player) = &*select_asr_mpv.borrow() else { return };
        let Some(media_ctx) = player.capture_encode_context() else {
            app.set_status_message("No media loaded or no audio track selected".into());
            return;
        };
        let config_dir = memento_xdg_dir("XDG_CONFIG_HOME", ".config")
            .unwrap_or_else(|| PathBuf::from("."));
        let settings = match controller::asr::AsrSettings::from_config(&config_dir, &*select_asr_config.store()) {
            Ok(s) => s,
            Err(err) => {
                app.set_status_message(err.into());
                return;
            }
        };
        let media = controller::asr::AsrMedia::new(media_ctx.path, media_ctx.audio_id);
        let pos = player.get_position();
        let dur = player.get_duration();
        match asr.start(settings, media, dur, pos) {
            Ok(()) => {
                let _ = player.set_sid(0);
                app.set_primary_sid(0);
                app.set_asr_running(true);
                select_asr_sub_ctrl.borrow_mut().ensure_track(0);
                app.set_status_message("ASR subtitles started…".into());
            }
            Err(err) => {
                app.set_status_message(err.into());
            }
        }
    });
    let auto_app = app.as_weak();
    let auto_config = config_ctrl.clone();
    let auto_cache = cache_dir.clone();
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
    let scale_cache = cache_dir.clone();
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
    let offset_cache = cache_dir.clone();
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

    let vol_mpv = mpv.clone();
    app.on_set_volume(move |vol| {
        if let Some(mpv_inst) = &*vol_mpv.borrow() {
            let _ = mpv_inst.set_volume(vol as f64);
        }
    });

    // ── Menu Bar & Shortcut Callbacks ───────────────────────────────────────
    let weak_mute = app.as_weak();
    let mute_mpv = mpv.clone();
    app.on_toggle_mute(move || {
        if let Some(app) = weak_mute.upgrade() {
            let vol = app.get_volume();
            let new_vol = if vol > 0.0 { 0.0 } else { 100.0 };
            app.set_volume(new_vol);
            if let Some(mpv_inst) = &*mute_mpv.borrow() {
                let _ = mpv_inst.set_volume(new_vol as f64);
            }
        }
    });

    let weak_vup = app.as_weak();
    let vup_mpv = mpv.clone();
    app.on_volume_up(move || {
        if let Some(app) = weak_vup.upgrade() {
            let new_vol = (app.get_volume() + 5.0).min(100.0);
            app.set_volume(new_vol);
            if let Some(mpv_inst) = &*vup_mpv.borrow() {
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
    let url_current = current_media.clone();
    let url_subs = sub_ctrl.clone();
    let url_generation = media_generation.clone();
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
            match load_media_into(&app, player, &url, &url_generation) {
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
    let reload_current = current_media.clone();
    let reload_subs = sub_ctrl.clone();
    let reload_generation = media_generation.clone();
    let reload_model = sub_model.clone();
    app.on_reload_media(move || {
        let Some(app) = reload_app.upgrade() else { return };
        let current = reload_current.borrow();
        let Some(path) = current.as_deref() else { return };
        if let Some(player) = &*reload_mpv.borrow() {
            match load_media_into(&app, player, path, &reload_generation) {
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
    let close_sub_ctrl = sub_ctrl.clone();
    let close_sub_model = sub_model.clone();
    let close_current = current_media.clone();
    let close_generation = media_generation.clone();
    let close_asr = asr_session.clone();
    app.on_close_media(move || {
        if let Some(mpv_inst) = &*close_mpv.borrow() {
            let _ = mpv_inst.command(&["stop"]);
        }
        close_asr.borrow_mut().cancel();
        close_generation.set(close_generation.get().wrapping_add(1));
        *close_current.borrow_mut() = None;
        close_sub_ctrl.borrow_mut().clear();
        close_sub_model.set_vec(Vec::new());
        if let Some(app) = weak_close.upgrade() {
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

    app.on_quit_app(|| {
        if let Err(error) = slint::quit_event_loop() {
            eprintln!("Could not close Memento: {error}");
        }
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
    let ocr_region_app = app.as_weak();
    let ocr_region_mpv = mpv.clone();
    let ocr_region_config = config_ctrl.clone();
    app.on_ocr_region(move |x, y, width, height| {
        let Some(app) = ocr_region_app.upgrade() else { return };
        app.set_status_message("Recognizing selected text…".into());
        let Some(controller) = ocr_controller.as_ref().ok() else { return };
        let Some(player) = &*ocr_region_mpv.borrow() else { return };
        let raw = match player.screenshot_raw_rgba(false) {
            Ok(raw) => raw,
            Err(error) => {
                app.set_status_message(format!("Cannot capture video: {error}").into());
                return;
            }
        };
        let frame = match controller::ocr::OcrFrame::from_rgba(
            raw.width, raw.height, raw.pixels,
        ) {
            Ok(frame) => frame,
            Err(error) => {
                app.set_status_message(error.to_string().into());
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
        let result = controller.read_region_async(
            frame,
            controller::ocr::OcrRegion {
                x: x.into(), y: y.into(), width: width.into(), height: height.into(),
            },
            controller::ocr::OcrViewport {
                width: app.get_video_width().into(),
                height: app.get_video_height().into(),
            },
            settings,
            move |recognized| {
                let _ = response_app.upgrade_in_event_loop(move |app| match recognized {
                    Ok(text) => {
                        let text = text.trim();
                        if !text.is_empty() {
                            app.set_dict_open(false);
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
            app.set_status_message(error.to_string().into());
        }
    });


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

    app.on_open_config(|| {
        if let Some(path) = memento_xdg_dir("XDG_CONFIG_HOME", ".config") {
            if let Err(error) = std::fs::create_dir_all(&path)
                .and_then(|_| Command::new("xdg-open").arg(path).spawn().map(|_| ()))
            {
                eprintln!("Cannot open the configuration directory: {error}");
            }
        }
    });


    let weak_fs = app.as_weak();
    app.on_toggle_fullscreen(move || {
        if let Some(app) = weak_fs.upgrade() {
            let cur = app.get_is_fullscreen();
            app.set_is_fullscreen(!cur);
            app.window().set_fullscreen(!cur);
        }
    });

    // ── Subtitle Drawer Row Selection ───────────────────────────────────────
    let sm = sub_model.clone();
    let sc = sub_ctrl.clone();
    app.on_select_row(move |row| {
        if row >= 0 && (row as usize) < sm.row_count() {
            sc.borrow_mut().toggle_select(row as usize);
            if let Some(mut entry) = sm.row_data(row as usize) {
                entry.selected = !entry.selected;
                sm.set_row_data(row as usize, entry);
            }
        }
    });
    let search_sub_ctrl = sub_ctrl.clone();
    let search_sub_model = sub_model.clone();
    app.on_search_subtitles(move |query| {
        let matching = search_sub_ctrl.borrow().search(&query, true);
        for idx in 0..search_sub_model.row_count() {
            if let Some(mut entry) = search_sub_model.row_data(idx) {
                entry.selected = matching.contains(&idx);
                search_sub_model.set_row_data(idx, entry);
            }
        }
    });

    // ── Dictionary Lookup ──────────────────────────────────────────────────
    let weak_dict = app.as_weak();
    let dc = dict_ctrl.clone();
    app.on_lookup_word(move |word| {
        if let Some(app) = weak_dict.upgrade() {
            let (hits, error, database_open, headword, reading, pitch, freq, jpdb) = {
                let Ok(mut dictionary) = dc.try_borrow_mut() else { return };
                let hits = dictionary.search(&word).to_vec();
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
                app.set_status_message(format!("Dictionary search failed: {error}").into());
            } else if hits.is_empty() && !database_open && !word.trim().is_empty() {
                app.set_status_message("Dictionary database is unavailable".into());
            }
            app.set_dict_results(Rc::new(VecModel::from(dictionary_result_rows(&hits))).into());
            app.set_dict_query(headword.clone().into());
            app.set_dict_reading(reading.into());
            app.set_dict_pitch_accent(pitch.into());
            app.set_dict_freq_rank(freq.into());
            app.set_dict_jpdb_rank(jpdb.into());
            app.set_dict_open(!hits.is_empty());

            // Multi-token match calculation so compounds and words highlight completely
            let mut match_count = 1;
            if !headword.is_empty() {
                let start_idx = app.get_selected_token_index();
                if start_idx >= 0 {
                    let sub_words = app.get_active_sub_words();
                    let target_len = headword.chars().count();
                    let mut accumulated = 0;
                    let mut count = 0;
                    for i in (start_idx as usize)..sub_words.row_count() {
                        if let Some(w) = sub_words.row_data(i) {
                            accumulated += w.text.chars().count();
                            count += 1;
                            if accumulated >= target_len {
                                break;
                            }
                        } else {
                            break;
                        }
                    }
                    if count > 0 {
                        match_count = count as i32;
                    }
                }
            }
            app.set_selected_token_count(match_count);
        }
    });
    let hover_timer = Rc::new(slint::Timer::default());
    let hover_pending = Rc::new(RefCell::new(None::<String>));
    let hover_app = app.as_weak();
    let hover_config = config_ctrl.clone();
    let hover_timer_callback = hover_timer.clone();
    let hover_pending_callback = hover_pending.clone();
    let hover_cursor_callback = hover_cursor.clone();
    app.on_hover_word(move |word, index, control, shift, alt, meta| {
        let Some(app) = hover_app.upgrade() else { return };
        hover_timer_callback.stop();
        if word.is_empty() {
            hover_cursor_callback.borrow_mut().take();
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
            return;
        }

        // If a word popup is currently open and an index was explicitly clicked, don't overwrite on hover
        if app.get_dict_open() && app.get_selected_token_index() >= 0 && !modifier_held {
            return;
        }

        let delay_ms = match hover_config.effective_setting(raw::search::GROUP, raw::search::DELAY) {
            Some(Value::Int(value)) => value.max(0) as u64,
            _ => raw::search::DELAY_DEFAULT as u64,
        };
        hover_pending_callback.replace(Some(word.to_string()));
        app.set_selected_token_index(index);
        let pending = hover_pending_callback.clone();
        let weak = app.as_weak();
        hover_timer_callback.start(
            slint::TimerMode::SingleShot,
            std::time::Duration::from_millis(delay_ms),
            move || {
                let Some(word) = pending.borrow_mut().take() else { return };
                let _ = weak.upgrade_in_event_loop(move |app| app.invoke_lookup_word(word.into()));
            },
        );
    });
    let search_dict_app = app.as_weak();
    let search_dict_ctrl = dict_ctrl.clone();
    app.on_search_dictionary(move |query| {
        let Some(app) = search_dict_app.upgrade() else { return };
        let hits = if query.trim().is_empty() {
            Vec::new()
        } else {
            let Ok(mut dictionary) = search_dict_ctrl.try_borrow_mut() else { return };
            let mut hits = dictionary.search(&query).to_vec();
            let mut error = dictionary.take_search_error();
            if let Some(first_character) = query.chars().next() {
                let kanji_hits = dictionary.search_kanji(&first_character.to_string()).to_vec();
                if error.is_none() {
                    error = dictionary.take_search_error();
                } else {
                    let _ = dictionary.take_search_error();
                }
                for hit in kanji_hits {
                    if !hits.iter().any(|existing| {
                        existing.expression == hit.expression
                            && existing.reading == hit.reading
                            && existing.dict_name == hit.dict_name
                    }) {
                        hits.push(hit);
                    }
                }
            }
            let database_open = dictionary.database_path_is_open();
            drop(dictionary);
            if let Some(error) = error {
                app.set_status_message(format!("Dictionary search failed: {error}").into());
            } else if hits.is_empty() && !database_open && !query.trim().is_empty() {
                app.set_status_message("Dictionary database is unavailable".into());
            }
            hits
        };
        app.set_dict_results(Rc::new(VecModel::from(dictionary_result_rows(&hits))).into());
        app.set_dict_open(false);
    });
    let kanji_app = app.as_weak();
    let kanji_dict = dict_ctrl.clone();
    app.on_lookup_kanji(move |word| {
        let Some(app) = kanji_app.upgrade() else { return };
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
        app.set_dict_results(Rc::new(VecModel::from(dictionary_result_rows(&hits))).into());
        app.set_dict_query(headword.into());
        app.set_dict_reading(reading.into());
        app.set_dict_pitch_accent(pitch.into());
        app.set_dict_freq_rank(freq.into());
        app.set_dict_jpdb_rank(jpdb.into());
        app.set_dict_open(!hits.is_empty());
    });
    let audio_player: Rc<RefCell<Option<controller::audio::AudioController>>> =
        Rc::new(RefCell::new(None));
    let audio_app = app.as_weak();
    let audio_config = config_ctrl.clone();
    app.on_play_audio_for_word(move |expression| {
        let Some(app) = audio_app.upgrade() else { return };
        let sources = audio_config.audio_source_entries();
        if sources.is_empty() {
            app.set_status_message("No pronunciation audio source is configured".into());
            return;
        }
        let reading = app.get_dict_reading().to_string();
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
            reading,
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
    let selection_app = app.as_weak();
    app.on_toggle_definition_selection(move |index| {
        let Some(app) = selection_app.upgrade() else { return };
        if index < 0 { return };
        let rows = app.get_dict_results();
        if let Some(mut row) = rows.row_data(index as usize) {
            row.selected = !row.selected;
            rows.set_row_data(index as usize, row);
        }
    });

    let note_app = app.as_weak();
    let note_config = config_ctrl.clone();
    app.on_add_anki_note(move |index| {
        let Some(app) = note_app.upgrade() else { return };
        if index < 0 { return }
        let Some(profile) = note_config.anki_profile_note_config() else {
            app.set_status_message("Select an Anki profile and configure a term note".into());
            return;
        };
        if !profile.enabled {
            app.set_status_message("Enable Anki integration in Preferences".into());
            return;
        }
        let definitions = app.get_dict_results();
        let Some(hit) = definitions.row_data(index as usize) else { return };
        let mut context = controller::anki::NoteFieldsContext::new(
            hit.expression.to_string(), hit.reading.to_string(),
            app.get_active_sub_text().to_string(),
        );
        context.dictionary_name = hit.dict_name.to_string();
        let selected: Vec<_> = (0..definitions.row_count())
            .filter_map(|row| definitions.row_data(row))
            .filter(|row| row.selected)
            .map(|row| row.glossary.to_string())
            .collect();
        context.glossary_items = if selected.is_empty() {
            vec![hit.glossary.to_string()]
        } else {
            selected
        };
        context.tags = hit.pos_tags.split_whitespace().map(str::to_owned).collect();
        app.set_status_message("Adding note to Anki…".into());
        let response_app = note_app.clone();
        std::thread::spawn(move || {
            let controller = AnkiController::new(profile.endpoint());
            let result = controller.add_note_from_profile(&profile, &context);
            let _ = response_app.upgrade_in_event_loop(move |app| {
                app.set_status_message(match result {
                    Ok(note_id) => format!("Added Anki note {note_id}").into(),
                    Err(error) => format!("Could not add Anki note: {error}").into(),
                });
            });
        });
    });
    // ── 100ms Playback Synchronization Timer ────────────────────────────────
    let timer = slint::Timer::default();
    let weak_timer_app = app.as_weak();
    let timer_mpv = mpv.clone();
    let timer_sub_ctrl = sub_ctrl.clone();
    let timer_sub_model = sub_model.clone();
    let timer_media_generation = media_generation.clone();
    let timer_asr = asr_session.clone();
    let mut previous_audio = Vec::new();
    let mut previous_tracks = Vec::new();
    let mut previous_lines = Vec::new();
    let mut previous_primary_text = String::new();
    let mut previous_secondary_text = String::new();
    let mut previous_sub_times = (0.0, 0.0);
    let mut previous_auto_pause = false;
    let mut auto_pause_state = memento_player::MementoAutoPauseStateC::default();
    let mut poll_number = 0usize;
    let mut observed_media_generation = u64::MAX;
    let mut status_message_ticks = 0usize;
    let mut last_observed_status = slint::SharedString::default();


    timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(100),
        move || {
            let Some(app) = weak_timer_app.upgrade() else { return };
            let Ok(player_ref) = timer_mpv.try_borrow() else { return };
            let Some(player) = player_ref.as_ref() else { return };
            let media_revision = timer_media_generation.get();
            if observed_media_generation != media_revision {
                observed_media_generation = media_revision;
                timer_asr.borrow_mut().cancel();
                app.set_asr_running(false);
                previous_audio.clear();
                previous_tracks.clear();
                previous_lines.clear();
                previous_primary_text.clear();
                previous_secondary_text.clear();
                previous_sub_times = (0.0, 0.0);
                previous_auto_pause = false;
                auto_pause_state = memento_player::MementoAutoPauseStateC::default();
                poll_number = 0;

                timer_sub_ctrl.borrow_mut().clear();
                timer_sub_model.set_vec(Vec::new());
                app.set_audio_tracks(Rc::new(VecModel::<PlayerTrack>::default()).into());
                app.set_subtitle_tracks(Rc::new(VecModel::<PlayerTrack>::default()).into());
                app.set_audio_id(0);
                app.set_primary_sid(0);
                app.set_secondary_sid(0);
                app.set_active_sub_text("".into());
                app.set_secondary_sub_text("".into());
                app.set_active_sub_words(Rc::new(VecModel::from(Vec::new())).into());
                app.set_active_sub_lines(Rc::new(VecModel::from(Vec::new())).into());
                app.set_selected_token_index(-1);
                app.set_dict_open(false);
            }
            let pos = player.get_position();
            let duration = player.get_duration();
            let paused = player.is_paused();

            app.set_position(pos as f32);
            app.set_duration(duration as f32);
            app.set_is_playing(!paused);
            app.set_volume(player.get_volume() as f32);
            app.set_current_time(format_time(pos).into());
            app.set_duration_time(format_time(duration).into());

            let current_status = app.get_status_message();
            if !current_status.is_empty() {
                if current_status != last_observed_status {
                    last_observed_status = current_status;
                    status_message_ticks = 0;
                } else {
                    status_message_ticks += 1;
                    if status_message_ticks >= 40 {
                        app.set_status_message("".into());
                        last_observed_status = "".into();
                        status_message_ticks = 0;
                    }
                }
            } else {
                last_observed_status = "".into();
                status_message_ticks = 0;
            }

            poll_number += 1;
            if poll_number >= 5 {
                poll_number = 0;
                sync_player_tracks(
                    &app, player, &mut timer_sub_ctrl.borrow_mut(),
                    &timer_sub_model, &mut previous_audio, &mut previous_tracks,
                );
            }

            let primary_sid = player.primary_sid();
            let secondary_sid = player.secondary_sid();
            let primary_text = player.get_sub_text().unwrap_or_default();
            let secondary_text = player.get_secondary_sub_text().unwrap_or_default();
            let raw_primary = player.get_sub_raw_time(pos);
            let raw_secondary = player.get_secondary_sub_raw_time(pos);
            let Ok(mut subtitles) = timer_sub_ctrl.try_borrow_mut() else { return };
            if primary_sid > 0 {
                subtitles.ensure_track(primary_sid);
            }
            if secondary_sid > 0 {
                subtitles.ensure_track(secondary_sid);
            }

            let current_lines = player.primary_sub_lines();
            if current_lines != previous_lines {
                let cues: Vec<MemorySubtitleCue> = current_lines.iter().map(|line| {
                    MemorySubtitleCue {
                        id: line.id,
                        text: line.text.encode_utf16().collect(),
                        start: line.start,
                        end: line.end,
                    }
                }).collect();
                if subtitles.update_memory_cues(primary_sid, &cues, raw_primary) {
                    refresh_subtitle_list(&timer_sub_model, &subtitles);
                }
                previous_lines = current_lines;
            }

            let primary_times = (player.get_sub_start(), player.get_sub_end());
            if primary_text != previous_primary_text || primary_times != previous_sub_times {
                let previous_count = subtitles.primary_entries().len();
                let cue = (!primary_text.is_empty()).then(|| {
                    controller::subtitles::create_cue(
                        &primary_text, player.get_sub_start(), player.get_sub_end(),
                    )
                });
                subtitles.update_active_cue(primary_sid, cue.as_ref(), raw_primary);
                if subtitles.primary_entries().len() != previous_count {
                    refresh_subtitle_list(&timer_sub_model, &subtitles);
                }
                app.set_active_sub_words(
                    Rc::new(VecModel::from(subtitle_words(&primary_text))).into(),
                );
                app.set_active_sub_lines(
                    Rc::new(VecModel::from(subtitle_lines(&primary_text))).into(),
                );
                app.set_selected_token_index(-1);
                app.set_active_sub_text(primary_text.clone().into());
                previous_primary_text = primary_text;
            }
            if secondary_text != previous_secondary_text {
                let cue = (!secondary_text.is_empty()).then(|| {
                    controller::subtitles::create_cue(
                        &secondary_text,
                        player.get_secondary_sub_start(),
                        player.get_secondary_sub_end(),
                    )
                });
                subtitles.update_active_cue(secondary_sid, cue.as_ref(), raw_secondary);
                app.set_secondary_sub_text(secondary_text.clone().into());
                previous_secondary_text = secondary_text;
            }
            if app.get_asr_running() {
                let mut asr = timer_asr.borrow_mut();
                asr.update_position(pos, duration, paused);
                let mut refreshed_subtitles = false;
                asr.poll_with(|event| match event {
                    controller::asr::AsrEvent::CueAdded { cue, .. } => {
                        let mem_cue = MemorySubtitleCue {
                            id: cue.id as i64,
                            text: cue.text.encode_utf16().collect(),
                            start: cue.start,
                            end: cue.end,
                        };
                        if subtitles.update_memory_cues(0, &[mem_cue], raw_primary) {
                            refreshed_subtitles = true;
                        }
                    }
                    controller::asr::AsrEvent::Completed { cue_count, .. } => {
                        app.set_status_message(format!("ASR transcription completed ({cue_count} cues)").into());
                    }
                    controller::asr::AsrEvent::Failed { message, .. } => {
                        app.set_status_message(format!("ASR error: {message}").into());
                        app.set_asr_running(false);
                    }
                    controller::asr::AsrEvent::Paused { message, .. } => {
                        app.set_status_message(format!("ASR paused: {message}").into());
                    }
                    controller::asr::AsrEvent::Canceled { .. } => {
                        app.set_asr_running(false);
                    }
                    _ => {}
                });
                if refreshed_subtitles {
                    refresh_subtitle_list(&timer_sub_model, &subtitles);
                }
                let asr_text = subtitles.active_cue_text().unwrap_or_default();
                if asr_text != previous_primary_text {
                    app.set_active_sub_words(
                        Rc::new(VecModel::from(subtitle_words(&asr_text))).into(),
                    );
                    app.set_active_sub_lines(
                        Rc::new(VecModel::from(subtitle_lines(&asr_text))).into(),
                    );
                    app.set_selected_token_index(-1);
                    app.set_active_sub_text(asr_text.clone().into());
                    previous_primary_text = asr_text;
                }
            }
            let before_active = subtitles.active_indices(primary_sid).to_vec();
            subtitles.update_positions(raw_primary, raw_secondary);
            refresh_active_rows(
                &timer_sub_model, &before_active, subtitles.active_indices(primary_sid),
            );
            drop(subtitles);

            unsafe {
                memento_player::memento_player_autopause_position(
                    &mut auto_pause_state, pos, player.get_sub_speed(), player.get_sub_delay(),
                );
            }
            let times = primary_times;
            let auto_pause = app.get_auto_pause();
            if times != previous_sub_times || auto_pause != previous_auto_pause {
                previous_sub_times = times;
                previous_auto_pause = auto_pause;
                let mut seek_time = 0.0;
                if unsafe {
                    memento_player::memento_player_autopause_step(
                        &mut auto_pause_state, auto_pause, times.0, times.1,
                        player.get_sub_speed(), player.get_sub_delay(),
                        paused, duration, &mut seek_time,
                    )
                } {
                    let _ = player.pause();
                    let _ = player.seek(seek_time);
                }
            }
        },
    );

    // libmpv's software render context writes the video into the Slint image.
    // We maintain a 3-buffer ring of SharedPixelBuffer. When a buffer is submitted
    // to Slint, Slint uploads it to GPU texture. By the time that slot is reused
    // 2 frames later, Slint has released its reference, so make_mut_bytes()
    // incurs zero copies and zero reallocations.
    // Rendering at native (vw, vh) bypasses CPU swscaling completely.
    let video_timer = slint::Timer::default();
    let video_app = app.as_weak();
    let video_mpv = mpv.clone();
    let mut frame_pool: [Option<SharedPixelBuffer<Rgba8Pixel>>; 3] = [None, None, None];
    let mut pool_idx = 0usize;
    let mut rendered_size = (0, 0);
    video_timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(16),
        move || {
            let Some(app) = video_app.upgrade() else { return };
            if app.get_video_width() <= 0 || app.get_video_height() <= 0 {
                return;
            }
            let Ok(player_ref) = video_mpv.try_borrow() else { return };
            let Some(player) = player_ref.as_ref() else { return };
            let (vw, vh) = player.video_dimensions();
            let (target_w, target_h) = if vw > 0 && vh > 0 {
                (vw, vh)
            } else {
                let scale = app.window().scale_factor();
                let w = ((app.get_video_width() as f32 * scale).round() as u32).max(1);
                let h = ((app.get_video_height() as f32 * scale).round() as u32).max(1);
                (w, h)
            };

            if rendered_size == (target_w, target_h) && !player.take_render_update() {
                return;
            }

            let mut buffer = match frame_pool[pool_idx].take() {
                Some(buf) if buf.width() == target_w && buf.height() == target_h => buf,
                _ => SharedPixelBuffer::<Rgba8Pixel>::new(target_w, target_h),
            };

            if let Err(error) = player.render_rgba(target_w, target_h, buffer.make_mut_bytes()) {
                eprintln!("Cannot render video frame: {error}");
                frame_pool[pool_idx] = Some(buffer);
                return;
            }

            app.set_video_frame(slint::Image::from_rgba8(buffer.clone()));
            frame_pool[pool_idx] = Some(buffer);
            pool_idx = (pool_idx + 1) % 3;
            rendered_size = (target_w, target_h);
        },
    );

    app.run()?;
    Ok(())
}

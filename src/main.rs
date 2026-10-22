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
    player.pause()?;
    player.load_file(path)?;
    media_generation.set(media_generation.get().wrapping_add(1));
    if player.is_opengl_active() {
        app.set_pending_playback_generation((media_generation.get() & i32::MAX as u64) as i32);
    } else {
        app.set_pending_playback_generation(-1);
        player.play()?;
    }
    app.set_is_playing(true);
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
        Self { gl, texture: None, framebuffer: None, size: (0, 0) }
    }

    fn ensure_size(&mut self, size: (u32, u32)) -> Result<(), String> {
        if self.size == size { return Ok(()); }
        let gl = &self.gl;
        unsafe {
            let old_framebuffer = gl.get_parameter_i32(glow::FRAMEBUFFER_BINDING);
            let old_texture = gl.get_parameter_i32(glow::TEXTURE_BINDING_2D);
            if self.texture.is_none() {
                self.texture = Some(gl.create_texture().map_err(|error| error.to_string())?);
            }
            if self.framebuffer.is_none() {
                self.framebuffer = Some(gl.create_framebuffer().map_err(|error| error.to_string())?);
            }
            let texture = self.texture.expect("created GL texture");
            let framebuffer = self.framebuffer.expect("created GL framebuffer");
            gl.bind_texture(glow::TEXTURE_2D, Some(texture));
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);
            gl.tex_image_2d(glow::TEXTURE_2D, 0, glow::RGBA as i32,
                size.0 as i32, size.1 as i32, 0, glow::RGBA, glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(None));
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(framebuffer));
            gl.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D, Some(texture), 0);
            let status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
            gl.bind_framebuffer(glow::FRAMEBUFFER,
                std::num::NonZeroU32::new(old_framebuffer as u32).map(glow::NativeFramebuffer));
            gl.bind_texture(glow::TEXTURE_2D,
                std::num::NonZeroU32::new(old_texture as u32).map(glow::NativeTexture));
            if status != glow::FRAMEBUFFER_COMPLETE {
                return Err(format!("video framebuffer is incomplete: 0x{status:x}"));
            }
            self.size = size;
        }
        Ok(())
    }

    fn texture_info(&self) -> Option<(std::num::NonZeroU32, u32, u32)> {
        self.texture.map(|texture| (texture.0, self.size.0, self.size.1))
    }

    fn render(&self, player: &MpvInstance) -> Result<(), String> {
        let Some(framebuffer) = self.framebuffer else { return Ok(()) };
        let gl = &self.gl;
        unsafe {
            let old_framebuffer = gl.get_parameter_i32(glow::FRAMEBUFFER_BINDING);
            let old_texture = gl.get_parameter_i32(glow::TEXTURE_BINDING_2D);
            let old_active_texture = gl.get_parameter_i32(glow::ACTIVE_TEXTURE);
            let old_program = gl.get_parameter_i32(glow::CURRENT_PROGRAM);
            let old_vertex_array = gl.get_parameter_i32(glow::VERTEX_ARRAY_BINDING);
            let old_scissor_enabled = gl.is_enabled(glow::SCISSOR_TEST);
            let mut old_scissor = [0; 4];
            gl.get_parameter_i32_slice(glow::SCISSOR_BOX, &mut old_scissor);
            let old_blend = [
                gl.get_parameter_i32(glow::BLEND_SRC_RGB),
                gl.get_parameter_i32(glow::BLEND_DST_RGB),
                gl.get_parameter_i32(glow::BLEND_SRC_ALPHA),
                gl.get_parameter_i32(glow::BLEND_DST_ALPHA),
            ];
            let mut old_viewport = [0; 4];
            gl.get_parameter_i32_slice(glow::VIEWPORT, &mut old_viewport);
            let result = video_gl::without_blending(gl, || {
                player.render_opengl(framebuffer.0.get(), self.size.0, self.size.1)
            });
            gl.bind_framebuffer(glow::FRAMEBUFFER,
                std::num::NonZeroU32::new(old_framebuffer as u32).map(glow::NativeFramebuffer));
            gl.active_texture(old_active_texture as u32);
            gl.bind_texture(glow::TEXTURE_2D,
                std::num::NonZeroU32::new(old_texture as u32).map(glow::NativeTexture));
            gl.use_program(std::num::NonZeroU32::new(old_program as u32).map(glow::NativeProgram));
            gl.bind_vertex_array(std::num::NonZeroU32::new(old_vertex_array as u32).map(glow::NativeVertexArray));
            gl.viewport(old_viewport[0], old_viewport[1], old_viewport[2], old_viewport[3]);
            gl.scissor(old_scissor[0], old_scissor[1], old_scissor[2], old_scissor[3]);
            if old_scissor_enabled { gl.enable(glow::SCISSOR_TEST); } else { gl.disable(glow::SCISSOR_TEST); }
            gl.blend_func_separate(old_blend[0] as u32, old_blend[1] as u32,
                old_blend[2] as u32, old_blend[3] as u32);
            result
        }
    }

    fn destroy(mut self) {
        unsafe {
            if let Some(framebuffer) = self.framebuffer.take() { self.gl.delete_framebuffer(framebuffer); }
            if let Some(texture) = self.texture.take() { self.gl.delete_texture(texture); }
        }
    }
}

fn video_render_size(app: &MementoApp, player: &MpvInstance) -> (u32, u32) {
    if app.get_video_width() <= 0 || app.get_video_height() <= 0 { return (0, 0); }
    let (vw, vh) = player.cached_video_dimensions();
    let scale = app.window().scale_factor();
    let viewport_w = ((app.get_video_width() as f32 * scale).round() as u32).max(1);
    let viewport_h = ((app.get_video_height() as f32 * scale).round() as u32).max(1);
    if vw > 0 && vh > 0 {
        let ratio = (viewport_w as f64 / vw as f64)
            .min(viewport_h as f64 / vh as f64).min(1.0);
        (((vw as f64 * ratio).round() as u32).max(1),
         ((vh as f64 * ratio).round() as u32).max(1))
    } else {
        (viewport_w, viewport_h)
    }
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

#[cfg(test)]
mod subtitle_layout_tests {
    use super::*;

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



fn sync_player_tracks(
    app: &MementoApp,
    state: &memento_player::mpv::PlaybackState,
    subtitles: &mut SubtitleController,
    subtitle_rows: &VecModel<SubEntry>,
    previous_audio: &mut Vec<memento_player::AudioTrack>,
    previous_subtitles: &mut Vec<memento_player::SubtitleTrack>,
) {
    let audio = &state.audio_tracks;
    if audio != previous_audio {
        let rows: Vec<PlayerTrack> = audio.iter().filter_map(|track| {
            Some(PlayerTrack {
                id: i32::try_from(track.id).ok()?,
                label: track_label(track.id, &track.language, &track.title).into(),
                selected: track.selected,
            })
        }).collect();
        app.set_audio_tracks(Rc::new(VecModel::from(rows)).into());
        previous_audio.clone_from(audio);
    }
    app.set_audio_id(i32::try_from(state.audio_id).unwrap_or(0));

    let tracks = &state.subtitle_tracks;
    if tracks != previous_subtitles {
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
        previous_subtitles.clone_from(tracks);
        refresh_subtitle_list(subtitle_rows, subtitles);
    }
    let primary = state.primary_sid;
    if subtitles.primary_sid() != primary {
        subtitles.set_primary_sid(primary);
        refresh_subtitle_list(subtitle_rows, subtitles);
    }
    let secondary = state.secondary_sid;
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
) {
    let restart = app.get_asr_running() && app.get_asr_enabled();
    asr.borrow_mut().cancel();
    let mut subtitles = subtitles.borrow_mut();
    subtitles.clear_asr_track();
    refresh_subtitle_list(subtitle_model, &subtitles);
    app.set_asr_running(false);
    app.set_active_sub_text("".into());
    app.set_active_sub_words(Rc::new(VecModel::from(Vec::new())).into());
    app.set_active_sub_lines(Rc::new(VecModel::from(Vec::new())).into());
    restart_pending.set(restart);
    app.set_selected_token_index(-1);
    app.set_dict_open(false);
}

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
                    clear_asr_for_settings(&app, &apply_asr, &apply_subtitles, &apply_subtitle_model, &apply_restart);
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
                    clear_asr_for_settings(&app, &reset_asr, &reset_subtitles, &reset_subtitle_model, &reset_restart);
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
    settings.on_defaults_settings(move |category| {
        let result = defaults_config.defaults_settings(category);
        if result.is_ok() && category == 8 {
            if let Some(app) = defaults_app.upgrade() {
                app.set_asr_enabled(controller::asr::AVAILABLE && setting_enabled(&defaults_config, raw::asr::GROUP, raw::asr::ENABLED, raw::asr::ENABLED_DEFAULT));
                clear_asr_for_settings(&app, &defaults_asr, &defaults_subtitles, &defaults_subtitle_model, &defaults_restart);
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
    let _ = log::set_logger(&APP_LOGGER);
    log::set_max_level(log::LevelFilter::Warn);
    let config_dir = memento_xdg_dir("XDG_CONFIG_HOME", ".config")
        .ok_or("HOME or XDG_CONFIG_HOME must be set to load Anki profiles")?;
    // Read effective mpv options before creating the UI's GL context. On a
    // hybrid system a direct decoder needs the matching rendering device.
    let mpv = MpvInstance::new_software(Some(&config_dir));
    if let Ok(player) = &mpv {
        graphics_device::select_for_decoder(player.configured_hwdec());
    }
    // libmpv shares the UI's context. On desktop platforms prefer desktop GL
    // with the driver's supported version, rather than Slint's GLES 2 default.
    // An explicitly selected Slint backend (including software) still wins.
    if std::env::var_os("SLINT_BACKEND").is_none() {
        slint::BackendSelector::new().require_opengl().select()?;
    }
    let app = MementoApp::new()?;
    ui::prepare_text_fonts();

    let cache_dir = memento_xdg_dir("XDG_CACHE_HOME", ".cache")
        .ok_or("HOME or XDG_CACHE_HOME must be set to load Memento settings")?;
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
    let mpv = match mpv {
        Ok(instance) => Rc::new(RefCell::new(Some(instance))),
        Err(err) => {
            eprintln!("Could not initialize video playback: {err}");
            Rc::new(RefCell::new(None))
        }
    };
    let video_timings = Rc::new(RefCell::new(VideoTimings::default()));
    let pending_playback_frame = Rc::new(Cell::new(None::<i32>));
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
    let open_cache = cache_dir.clone();
    let open_anki = anki_json_path.clone();
    let open_app = app.as_weak();
    let open_asr_session = asr_session.clone();
    let open_subtitles = sub_ctrl.clone();
    let open_subtitle_model = sub_model.clone();
    let open_asr_restart = asr_restart_pending.clone();
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

    let current_media: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let media_generation = Rc::new(Cell::new(0u64));
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
    let key_hover_timer = hover_timer.clone();
    let key_hover_pending = hover_pending.clone();
    let key_lookup_epoch = lookup_epoch.clone();
    let key_mpv = mpv.clone();
    let shortcut_app = app.as_weak();
    app.on_key_pressed(move |text, control, shift, alt, meta| {
        let Some(app) = shortcut_app.upgrade() else { return false };
        // Any keyboard input dismisses a definition, even when the key is
        // handled by the player or a child widget.
        let had_dict_open = app.get_dict_open();
        app.set_dict_open(false);
        key_lookup_epoch.fetch_add(1, Ordering::AcqRel);
        key_hover_timer.stop();
        key_hover_pending.borrow_mut().take();
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
    let last_pointer = Rc::new(Cell::new(None::<(f32, f32)>));
    app.on_pointer_moved(move |x, y| {
        let Some(app) = pointer_app.upgrade() else { return };
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
    let activity_app = app.as_weak();
    let activity_osc_timer = osc_timer.clone();
    app.on_osc_activity(move || {
        let Some(app) = activity_app.upgrade() else { return };
        app.set_osc_visible(true);
        activity_osc_timer.set_interval(std::time::Duration::from_millis(app.get_osc_duration_ms() as u64));
        activity_osc_timer.restart();
    });

    let initial_file = args.iter().skip(1).find(|a| !a.starts_with('-')).cloned();

    if let Some(file_str) = initial_file {
        let initial_app = app.as_weak();
        let initial_mpv = mpv.clone();
        let initial_generation = media_generation.clone();
        let initial_media = current_media.clone();
        let initial_config = config_ctrl.clone();
        let initial_cache = cache_dir.clone();
        // The first window render initializes the OpenGL context. Load the
        // command-line media only after that render has had time to run.
        slint::Timer::single_shot(std::time::Duration::from_millis(100), move || {
            let Some(app) = initial_app.upgrade() else { return };
            let player_ref = initial_mpv.borrow();
            let Some(player) = player_ref.as_ref() else { return };
            match load_media_into(&app, player, &file_str, &initial_generation) {
                Ok(()) => {
                    *initial_media.borrow_mut() = Some(file_str.clone());
                    if Path::new(&file_str).is_file() {
                        if let Err(error) = record_recent_media(&app, &initial_config, &initial_cache, &file_str) {
                            eprintln!("Cannot save recent media: {error}");
                        }
                    }
                }
                Err(error) => eprintln!("Cannot open {}: {error}", file_str),
            }
        });
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
    let primary_sub_model = sub_model.clone();
    let primary_app = app.as_weak();
    let primary_asr = asr_session.clone();
    app.on_select_subtitle_track(move |id| {
        if let Some(app) = primary_app.upgrade() {
            if app.get_asr_running() {
                primary_asr.borrow_mut().cancel();
                app.set_asr_running(false);
                let mut subtitles = primary_subs.borrow_mut();
                subtitles.clear_asr_track();
                refresh_subtitle_list(&primary_sub_model, &subtitles);
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
    let select_asr_sub_model = sub_model.clone();
    app.on_select_asr(move || {
        let Some(app) = select_asr_app.upgrade() else { return };
        let mut asr = select_asr_session.borrow_mut();
        if app.get_asr_running() {
            asr.cancel();
            app.set_asr_running(false);
            let mut subtitles = select_asr_sub_ctrl.borrow_mut();
            subtitles.clear_asr_track();
            refresh_subtitle_list(&select_asr_sub_model, &subtitles);
            app.set_active_sub_text("".into());
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
        let gpu_unavailable = settings.backend == controller::asr::AsrBackend::Whisper
            && settings.whisper.use_gpu
            && settings.whisper.gpu_device.max(0) as usize >= controller::asr::whisper_gpu_device_count();
        let media = controller::media::MediaContext::new(media_ctx.path, media_ctx.audio_id);
        let pos = player.get_position();
        let dur = player.get_duration();
        match asr.start(settings, media, dur, pos) {
            Ok(()) => {
                if let Err(error) = player.set_sid(0) {
                    asr.cancel();
                    app.set_status_message(format!("Could not select ASR subtitles: {error}").into());
                    return;
                }
                let mut subtitles = select_asr_sub_ctrl.borrow_mut();
                subtitles.start_asr_track();
                refresh_subtitle_list(&select_asr_sub_model, &subtitles);
                app.set_primary_sid(0);
                app.set_asr_running(true);
                app.set_status_message(if gpu_unavailable {
                    "Whisper GPU unavailable; transcribing on CPU. Check GPU driver and CUDA/Vulkan build."
                } else {
                    "ASR subtitles started…"
                }.into());
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
    let furigana_app = app.as_weak();
    let furigana_config = config_ctrl.clone();
    let furigana_cache = cache_dir.clone();
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
            app.set_pending_playback_generation(-1);
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
    #[cfg(feature = "ocr")]
    {
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
    let popup_audio_cache: Arc<Mutex<PopupAudioState>> = Arc::new(Mutex::new(PopupAudioState::default()));
    let (lookup_tx, lookup_rx) = std::sync::mpsc::channel::<(u64, String, Vec<i64>, String, Vec<String>)>();
    let worker_epoch = lookup_epoch.clone();
    let lookup_path = dictionary_database_path.clone();
    let worker_app = app.as_weak();
    std::thread::spawn(move || {
        let mut database_signature = std::fs::metadata(&lookup_path).ok()
            .map(|metadata| (metadata.len(), metadata.modified().ok()));
        let mut dictionary = DictController::open(&lookup_path).unwrap_or_else(|error| {
            eprintln!("Dictionary lookup unavailable: {error}");
            DictController::new()
        });
        while let Ok(mut request) = lookup_rx.recv() {
            // Hover can produce several searches while a slow dictionary is
            // busy. Only the most recent query is useful to the popup.
            while let Ok(newer) = lookup_rx.try_recv() { request = newer; }
            if request.0 != worker_epoch.load(Ordering::Acquire) { continue; }
            let signature = std::fs::metadata(&lookup_path).ok()
                .map(|metadata| (metadata.len(), metadata.modified().ok()));
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
        app.set_dict_open(false);
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
            let selected = subtitles.selected_cues();
            let start = active.map(|cue| cue.start).unwrap_or(0.0);
            let end = active.map(|cue| cue.end).unwrap_or(0.0);
            let context_start = selected.iter().map(|cue| cue.start).reduce(f64::min).unwrap_or(start);
            let context_end = selected.iter().map(|cue| cue.end).reduce(f64::max).unwrap_or(end);
            (start, end, context_start, context_end)
        };
        let wants_screenshot = profile.field_templates.values().any(|value| value.contains("{screenshot}") || value.contains("{screenshot:") || value.contains("{image}") || value.contains("{image:"));
        let wants_video_screenshot = profile.field_templates.values().any(|value| value.contains("{screenshot-video}" ) || value.contains("{screenshot-video:"));
        let media_input = {
            let player_ref = note_mpv.borrow();
            let player = player_ref.as_ref();
            let media = player.and_then(MpvInstance::capture_encode_context)
                .map(|ctx| controller::media::MediaContext::new(ctx.path, ctx.audio_id));
            let mpv_start = player.map(MpvInstance::get_sub_start).unwrap_or(0.0);
            let mpv_end = player.map(MpvInstance::get_sub_end).unwrap_or(0.0);
            let subtitle_start = if active_end > active_start { active_start } else { mpv_start };
            let subtitle_end = if active_end > active_start { active_end } else { mpv_end };
            let screenshot = if wants_screenshot { player.and_then(|p| p.screenshot_raw_rgba(true).ok()) } else { None };
            let screenshot_video = if wants_video_screenshot { player.and_then(|p| p.screenshot_raw_rgba(false).ok()) } else { None };
            controller::anki_media::NoteMediaInput {
                media,
                subtitle_start,
                subtitle_end,
                context_start: if context_end > context_start { context_start } else { subtitle_start },
                context_end: if context_end > context_start { context_end } else { subtitle_end },
                screenshot,
                screenshot_video,
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
            let result = controller::anki_media::prepare_note_media(&profile, &mut context, media_input)
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
                    Err(error) => format!("Could not add Anki note: {error}").into(),
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
    // ── 100ms Playback Synchronization Timer ────────────────────────────────
    let timer = slint::Timer::default();
    let weak_timer_app = app.as_weak();
    let timer_mpv = mpv.clone();
    let timer_sub_ctrl = sub_ctrl.clone();
    let timer_sub_model = sub_model.clone();
    let timer_media_generation = media_generation.clone();
    let timer_lookup_epoch = lookup_epoch.clone();
    let timer_asr = asr_session.clone();
    let timer_asr_config = config_ctrl.clone();
    let timer_asr_restart = asr_restart_pending.clone();
    let timer_hover_owned = hover_pause_owned.clone();
    let timer_furigana_sender = furigana_sender.clone();
    let timer_furigana_epoch = furigana_epoch.clone();
    let mut previous_audio = Vec::new();
    let mut previous_tracks = Vec::new();
    let mut previous_lines = Vec::new();
    let mut previous_primary_text = String::new();
    let mut previous_secondary_text = String::new();
    let mut previous_sub_times = (0.0, 0.0);
    let mut previous_auto_pause = false;
    let mut previous_mpv_subtitles_hidden: Option<bool> = None;
    let mut previous_furigana_visible = app.get_furigana_visible();
    let mut previous_subtitle_layout: Option<(i32, i32, u32, bool)> = None;
    let mut auto_pause_state = memento_player::MementoAutoPauseStateC::default();
    let mut poll_number = 0usize;
    let mut observed_media_generation = u64::MAX;
    let mut status_message_ticks = 0usize;
    let mut last_observed_status = slint::SharedString::default();
    let timer_video_timings = video_timings.clone();
    let timer_pending_frame = pending_playback_frame.clone();
    let verbose_video_timings = std::env::var_os("MEMENTO_VIDEO_STATS").is_some();
    let mut last_video_report = std::time::Instant::now();
    let mut previous_drops = (0, 0);
    let mut last_sync = std::time::Instant::now();
    let mut previous_video_dimensions = (0, 0);


    timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(100),
        move || {
            let sync_started = std::time::Instant::now();
            {
                let mut timings = timer_video_timings.borrow_mut();
                timings.sync_gap_max = timings.sync_gap_max.max(sync_started.duration_since(last_sync));
            }
            last_sync = sync_started;
            let _sync_timing = SyncTiming { started: sync_started, timings: timer_video_timings.clone() };
            let Some(app) = weak_timer_app.upgrade() else { return };
            let Ok(player_ref) = timer_mpv.try_borrow() else { return };
            let Some(player) = player_ref.as_ref() else { return };
            let state = player.poll_playback_state();
            if app.get_pending_playback_generation() >= 0 && state.playback_ready {
                if player.is_opengl_active() {
                    // Metadata/subtitle UI updates in this tick are painted before
                    // the rendering notifier releases the playback clock.
                    timer_pending_frame.set(Some(app.get_pending_playback_generation()));
                    app.window().request_redraw();
                } else {
                    // A GL failure during loading must not leave the software
                    // fallback permanently paused.
                    app.set_pending_playback_generation(-1);
                    let _ = player.play();
                }
            }
            if last_video_report.elapsed() >= std::time::Duration::from_secs(5) {
                let drops = (state.frame_drops, state.decoder_drops);
                timer_video_timings.borrow_mut().report(&state, drops != previous_drops, verbose_video_timings, player.pending_render_delay());
                previous_drops = drops;
                last_video_report = std::time::Instant::now();
            }
            let media_revision = timer_media_generation.get();
            if observed_media_generation != media_revision {
                observed_media_generation = media_revision;
                timer_asr_restart.set(false);
                timer_asr.borrow_mut().cancel();
                app.set_asr_running(false);
                previous_audio.clear();
                previous_tracks.clear();
                previous_lines.clear();
                previous_primary_text.clear();
                previous_secondary_text.clear();
                previous_sub_times = (0.0, 0.0);
                previous_auto_pause = false;
                previous_mpv_subtitles_hidden = None;
                timer_hover_owned.set(false);
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
            let pos = state.position;
            let duration = state.duration;
            let paused = state.paused;

            if timer_asr_restart.get() {
                if !app.get_asr_enabled() {
                    timer_asr_restart.set(false);
                } else if let Some(media_ctx) = player.capture_encode_context() {
                    let config_dir = memento_xdg_dir("XDG_CONFIG_HOME", ".config")
                        .unwrap_or_else(|| PathBuf::from("."));
                    match controller::asr::AsrSettings::from_config(&config_dir, &*timer_asr_config.store()) {
                        Ok(settings) => {
                            let media = controller::media::MediaContext::new(media_ctx.path, media_ctx.audio_id);
                            match timer_asr.borrow_mut().start(settings, media, duration, pos) {
                                Ok(()) => {
                                    timer_asr_restart.set(false);
                                    if let Err(error) = player.set_sid(0) {
                                        timer_asr.borrow_mut().cancel();
                                        app.set_status_message(format!("Could not restart ASR subtitles: {error}").into());
                                    } else {
                                        let mut subtitles = timer_sub_ctrl.borrow_mut();
                                        subtitles.start_asr_track();
                                        refresh_subtitle_list(&timer_sub_model, &subtitles);
                                        app.set_primary_sid(0);
                                        app.set_asr_running(true);
                                        app.set_status_message("ASR model reloaded; transcribing subtitles…".into());
                                    }
                                }
                                Err(error) if error.contains("still stopping") => {},
                                Err(error) => {
                                    timer_asr_restart.set(false);
                                    app.set_status_message(format!("Could not restart ASR subtitles: {error}").into());
                                }
                            }
                        }
                        Err(error) => {
                            timer_asr_restart.set(false);
                            app.set_status_message(format!("Could not reload ASR model: {error}").into());
                        }
                    }
                } else {
                    timer_asr_restart.set(false);
                }
            }

            app.set_position(pos as f32);
            app.set_duration(duration as f32);
            app.set_is_playing(!paused || app.get_pending_playback_generation() >= 0);
            app.set_volume(state.volume as f32);
            app.set_current_time(format_time(pos).into());
            app.set_duration_time(format_time(duration).into());
            if previous_video_dimensions != state.dimensions {
                previous_video_dimensions = state.dimensions;
                app.invoke_update_video();
            }
            // A notification can arrive before the first usable viewport or
            // while the window is being reconfigured. Retry a stale redraw;
            // normal frame delivery still uses mpv's event-driven callback.
            if !paused && player.pending_render_delay() > std::time::Duration::from_millis(100) {
                app.invoke_video_frame_ready();
            }

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
                    &app, &state, &mut timer_sub_ctrl.borrow_mut(),
                    &timer_sub_model, &mut previous_audio, &mut previous_tracks,
                );
            }

            let primary_sid = state.primary_sid;
            let secondary_sid = state.secondary_sid;
            let primary_text = state.sub_text.clone();
            let secondary_text = state.secondary_sub_text.clone();
            let raw_primary = (pos - state.sub_delay) / state.sub_speed;
            let raw_secondary = (pos - state.secondary_sub_delay) / state.sub_speed;
            let Ok(mut subtitles) = timer_sub_ctrl.try_borrow_mut() else { return };
            if primary_sid > 0 {
                subtitles.ensure_track(primary_sid);
            }
            if secondary_sid > 0 {
                subtitles.ensure_track(secondary_sid);
            }

            let current_lines = &state.sub_lines;
            if current_lines != &previous_lines {
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
                previous_lines.clone_from(current_lines);
            }

            let primary_times = (state.sub_start, state.sub_end);
            if !app.get_asr_running()
                && (primary_text != previous_primary_text || primary_times != previous_sub_times) {
                app.set_dict_open(false);
                timer_lookup_epoch.fetch_add(1, Ordering::AcqRel);
                let previous_count = subtitles.primary_entries().len();
                let cue = (!primary_text.is_empty()).then(|| {
                    controller::subtitles::create_cue(
                        &primary_text, state.sub_start, state.sub_end,
                    )
                });
                subtitles.update_active_cue(primary_sid, cue.as_ref(), raw_primary);
                if subtitles.primary_entries().len() != previous_count {
                    refresh_subtitle_list(&timer_sub_model, &subtitles);
                }
                publish_active_subtitle(&app, &primary_text, &timer_furigana_sender, &timer_furigana_epoch);
                previous_primary_text = primary_text;
            }
            if secondary_text != previous_secondary_text {
                let cue = (!secondary_text.is_empty()).then(|| {
                    controller::subtitles::create_cue(
                        &secondary_text,
                        state.secondary_sub_start,
                        state.secondary_sub_end,
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
                        if subtitles.add_asr_cue(mem_cue, pos) {
                            refreshed_subtitles = true;
                        }
                    }
                    controller::asr::AsrEvent::CueRemoved { cue_id, .. } => {
                        if subtitles.remove_asr_cue(cue_id as i64, pos) {
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
            }
            let before_active = subtitles.active_indices(primary_sid).to_vec();
            subtitles.update_positions(if app.get_asr_running() { pos } else { raw_primary }, raw_secondary);
            refresh_active_rows(
                &timer_sub_model, &before_active, subtitles.active_indices(primary_sid),
            );
            if app.get_asr_running() {
                let asr_text = subtitles.active_cue_text().unwrap_or_default();
                if asr_text != previous_primary_text {
                    app.set_dict_open(false);
                    timer_lookup_epoch.fetch_add(1, Ordering::AcqRel);
                    publish_active_subtitle(&app, &asr_text, &timer_furigana_sender, &timer_furigana_epoch);
                    previous_primary_text = asr_text;
                }
            }
            if previous_furigana_visible != app.get_furigana_visible() {
                previous_furigana_visible = app.get_furigana_visible();
                publish_active_subtitle(&app, &app.get_active_sub_text(), &timer_furigana_sender, &timer_furigana_epoch);
            }
            let subtitle_layout = (
                app.get_video_width(), app.get_video_height(),
                app.get_subtitle_scale().to_bits(), app.get_furigana_visible(),
            );
            if previous_subtitle_layout != Some(subtitle_layout) {
                previous_subtitle_layout = Some(subtitle_layout);
                let words: Vec<_> = app.get_active_sub_words().iter().collect();
                if !words.is_empty() {
                    let lines = subtitle_lines_for_app(&app, words);
                    app.set_active_sub_lines(Rc::new(VecModel::from(lines)).into());
                }
            }
            drop(subtitles);

            if !app.get_hover_pause_enabled() && timer_hover_owned.replace(false) {
                let _ = player.play();
            }
            let memento_subtitles_visible = app.get_subtitles_visible()
                && !(app.get_hide_memento_while_playing() && !paused)
                && (!app.get_active_sub_text().is_empty() || !app.get_secondary_sub_text().is_empty());
            let hide_mpv = app.get_hide_mpv_subtitles() && memento_subtitles_visible;
            if previous_mpv_subtitles_hidden != Some(hide_mpv) {
                let visibility = if hide_mpv { "no" } else { "yes" };
                let primary = player.command_async(&["set", "sub-visibility", visibility]);
                let secondary = player.command_async(&["set", "secondary-sub-visibility", visibility]);
                previous_mpv_subtitles_hidden = Some(hide_mpv);
                if primary.is_err() || secondary.is_err() {
                    eprintln!("Could not update mpv subtitle visibility: {primary:?}; {secondary:?}");
                }
            }

            unsafe {
                memento_player::memento_player_autopause_position(
                    &mut auto_pause_state, pos, state.sub_speed, state.sub_delay,
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
                        state.sub_speed, state.sub_delay,
                        paused, duration, &mut seek_time,
                    )
                } {
                    let _ = player.pause();
                    let _ = player.seek(seek_time);
                }
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
    let notifier_texture = gl_ready_texture.clone();
    let notifier_app = app.as_weak();
    let notifier_timings = video_timings.clone();
    let notifier_pending_frame = pending_playback_frame.clone();
    if let Err(error) = app.window().set_rendering_notifier(move |state, graphics_api| {
        let slint::GraphicsAPI::NativeOpenGL { get_proc_address } = graphics_api else { return };
        match state {
            slint::RenderingState::RenderingSetup => {
                let player_ref = gl_mpv.borrow();
                let Some(player) = player_ref.as_ref() else { return };
                match player.activate_opengl(*get_proc_address) {
                    Ok(()) => {
                        *notifier_surface.borrow_mut() = Some(GlVideoSurface::new(*get_proc_address, player));
                        notifier_pending.set(true);
                        eprintln!("Video renderer: libmpv OpenGL");
                    }
                    Err(error) => eprintln!("OpenGL video unavailable; using software rendering: {error}"),
                }
            }
            slint::RenderingState::BeforeRendering => {
                {
                    let mut timings = notifier_timings.borrow_mut();
                    timings.paint_started = Some(std::time::Instant::now());
                    timings.paint_popup_rows = notifier_app.upgrade()
                        .filter(|app| app.get_dict_open())
                        .map_or(0, |app| app.get_popup_results().row_count());
                }
                let player_ref = gl_mpv.borrow();
                let Some(player) = player_ref.as_ref() else { return };
                let mut surface_ref = notifier_surface.borrow_mut();
                let Some(surface) = surface_ref.as_mut() else { return };
                let size = notifier_size.get();
                if size == (0, 0) { return; }
                // All mpv_render_* GL calls require this context to be current.
                let frame_ready = player.take_render_update();
                if !notifier_pending.replace(false) && !frame_ready && surface.size == size { return; }
                let render_started = std::time::Instant::now();
                if let Err(error) = surface.ensure_size(size).and_then(|()| surface.render(player)) {
                    eprintln!("OpenGL video failed; switching to software rendering: {error}");
                    notifier_texture.set(None);
                    if let Some(old_surface) = surface_ref.take() { old_surface.destroy(); }
                    if let Err(fallback_error) = player.fallback_to_software() {
                        eprintln!("Software video fallback failed: {fallback_error}");
                    }
                    let _ = notifier_app.upgrade_in_event_loop(|app| app.invoke_update_video());
                } else {
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
                if frame_ready { timings.queue_max = timings.queue_max.max(player.render_update_delay()); }
            }
            slint::RenderingState::AfterRendering => {
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
                if let Some(player) = gl_mpv.borrow().as_ref() { player.deactivate_opengl(); }
                if let Some(surface) = notifier_surface.borrow_mut().take() { surface.destroy(); }
            }
            _ => {}
        }
    }) {
        eprintln!("OpenGL video integration unavailable; using software rendering: {error}");
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
    let mut published_gl_texture = None;
    let mut rendered_size = (0, 0);
    app.on_update_video(move || {
            let Some(app) = video_app.upgrade() else { return };
            if app.get_video_width() <= 0 || app.get_video_height() <= 0 {
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
                let size = video_render_size(&app, player);
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
            let (mut target_w, mut target_h) = video_render_size(&app, player);
            // mpv's software API converts and scales on one CPU thread. Cap
            // fallback output area so a 1080p window does not upload 8 MB on
            // every frame if GL cannot be used.
            let area = u64::from(target_w) * u64::from(target_h);
            if area > 1280 * 720 {
                let ratio = ((1280 * 720) as f64 / area as f64).sqrt();
                target_w = ((target_w as f64 * ratio).round() as u32).max(1);
                target_h = ((target_h as f64 * ratio).round() as u32).max(1);
            }

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
    drop(render_worker);
    drop(render_request_tx);
    run_result?;
    Ok(())
}

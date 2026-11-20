//! Settings accessors, dictionary matcher wiring, recent-file title cache,
//! debounced subtitle scale/offset persistence, and preference refresh hooks.

use std::cell::{Cell, RefCell};
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use memento_setting::enums::ThemeMode;
use slint::{ComponentHandle, VecModel};

use crate::controller::asr::AsrController;
use crate::controller::config::ConfigController;
use crate::controller::dict::DictController;
use crate::controller::dictmanager::DictionaryManager;
use crate::controller::player::{effective_media_title, LivePlayerCoordinator};
use crate::controller::subtitles::SubtitleController;
use crate::settings_ui::{
    create_settings_session, refresh_category_settings, SettingsSession,
};
use crate::ui::{self, MementoApp, PopupBridge, SubEntry, SubtitleOverlayBridge};
use memento_config::{ArgbColor, QtFontSpec, Value};
use memento_os::SystemColorScheme;
use memento_player::MpvInstance;
use memento_setting::keys as raw;

const MAX_RECENT_TITLES: usize = 64;
pub(crate) const SETTINGS_IDLE_TEARDOWN_DELAY: Duration = Duration::from_secs(75);
pub(crate) const SETTINGS_IDLE_RETRY_DELAY: Duration = Duration::from_secs(45);
const DEFAULT_SUB_LIST_FONT_SIZE: f32 = if cfg!(target_os = "macos") {
    20.0
} else {
    16.0
};

static AUDIO_CONFIG_INITIALIZED: AtomicBool = AtomicBool::new(false);
static LAST_AUDIO_CONFIG_HASH: AtomicU64 = AtomicU64::new(0);
static AUDIO_CONFIG_GENERATION: AtomicU64 = AtomicU64::new(0);

/// Sentinel value (`NaN` bit pattern) indicating the furigana ratio cache has not been populated yet.
const FURIGANA_RATIO_UNSET: u32 = u32::MAX;
static CACHED_FURIGANA_RATIO_BITS: AtomicU32 = AtomicU32::new(FURIGANA_RATIO_UNSET);

/// Return the cached `interface/sub-furigana-ratio` value without touching `ConfigController`
/// on the 60 Hz playback sync tick. Updated whenever `refresh_player_preferences` runs.
pub(crate) fn cached_furigana_ratio() -> f32 {
    let bits = CACHED_FURIGANA_RATIO_BITS.load(Ordering::Relaxed);
    if bits == FURIGANA_RATIO_UNSET {
        raw::interface::subtitle::FURIGANA_RATIO_DEFAULT as f32
    } else {
        f32::from_bits(bits)
    }
}

fn store_cached_furigana_ratio(ratio: f32) {
    let bits = ratio.to_bits();
    if bits != FURIGANA_RATIO_UNSET {
        CACHED_FURIGANA_RATIO_BITS.store(bits, Ordering::Relaxed);
    }
}

#[derive(Default)]
struct BoundedRecentTitles {
    order: VecDeque<String>,
    map: HashMap<String, String>,
}

impl BoundedRecentTitles {
    fn insert(&mut self, path: String, title: String) {
        if let Some(existing) = self.map.get_mut(&path) {
            *existing = title;
            if let Some(pos) = self.order.iter().position(|p| p == &path) {
                self.order.remove(pos);
            }
            self.order.push_back(path);
            return;
        }
        while self.map.len() >= MAX_RECENT_TITLES {
            if let Some(oldest) = self.order.pop_front() {
                self.map.remove(&oldest);
            } else {
                self.map.clear();
                break;
            }
        }
        self.order.push_back(path.clone());
        self.map.insert(path, title);
    }

    fn get(&self, path: &str) -> Option<&str> {
        self.map.get(path).map(String::as_str)
    }

    fn clear(&mut self) {
        self.order.clear();
        self.map.clear();
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.map.len()
    }
}

static RECENT_TITLES: OnceLock<Mutex<BoundedRecentTitles>> = OnceLock::new();

fn recent_titles_cache() -> &'static Mutex<BoundedRecentTitles> {
    RECENT_TITLES.get_or_init(|| Mutex::new(BoundedRecentTitles::default()))
}

pub fn record_recent_media_title(path: &str, media_title: &str) {
    if path.trim().is_empty() {
        return;
    }
    let effective = effective_media_title(media_title, path);
    if !effective.is_empty() {
        if let Ok(mut cache) = recent_titles_cache().lock() {
            cache.insert(path.to_owned(), effective);
        }
    }
}

pub(crate) fn clear_recent_titles() {
    recent_titles_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
}

pub fn format_recent_file_label(media_title: &str, path: &str) -> String {
    let title = effective_media_title(media_title, path);
    if title.is_empty() || title == path {
        path.to_owned()
    } else {
        format!("{title} — {path}")
    }
}

pub(crate) fn record_recent_media(
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

pub(crate) fn refresh_recent_files(app: &MementoApp, config: &ConfigController) {
    let raw_files = config.recent_files();
    let titles = recent_titles_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let files: Vec<slint::SharedString> = raw_files.iter().cloned().map(Into::into).collect();
    let labels: Vec<slint::SharedString> = raw_files
        .iter()
        .map(|path| format_recent_file_label(titles.get(path).unwrap_or(""), path).into())
        .collect();
    drop(titles);
    app.set_recent_files(Rc::new(VecModel::from(files)).into());
    app.set_recent_file_labels(Rc::new(VecModel::from(labels)).into());
}

pub(crate) fn setting_number(
    config: &ConfigController,
    group: &str,
    key: &str,
    default: f64,
) -> f32 {
    match config.effective_setting(group, key) {
        Some(Value::Real(value)) => value as f32,
        Some(Value::Int(value)) => value as f32,
        _ => default as f32,
    }
}

pub(crate) fn setting_enabled(
    config: &ConfigController,
    group: &str,
    key: &str,
    default: bool,
) -> bool {
    match config.effective_setting(group, key) {
        Some(Value::Bool(value)) => value,
        Some(Value::Int(value)) => value != 0,
        Some(Value::Text(value)) => match value.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "on" => true,
            "false" | "0" | "no" | "off" => false,
            _ => default,
        },
        _ => default,
    }
}

pub(crate) fn setting_text(
    config: &ConfigController,
    group: &str,
    key: &str,
    default: &str,
) -> String {
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

/// Extended font attributes parsed from Qt `QFont::toString()` CSV strings
/// (`family,pointSizeF,pixelSize,styleHint,weight,style,underline,strikeOut,fixedPitch,rawMode[,...]`).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ParsedFontAttributes {
    pub(crate) family: String,
    pub(crate) size: f32,
    pub(crate) bold: bool,
    pub(crate) italic: bool,
    pub(crate) underline: bool,
    pub(crate) strikeout: bool,
    pub(crate) letter_spacing: f32,
}

pub(crate) fn parse_font_attributes(
    raw_font: &str,
    default_family: &str,
    default_size: f32,
    min_size: f32,
    max_size: f32,
) -> ParsedFontAttributes {
    let trimmed = raw_font.trim();
    if let Ok(spec) = QtFontSpec::parse(trimmed) {
        let parts: Vec<&str> = trimmed.split(',').collect();
        let parse_flag = |idx: usize| -> bool {
            parts.get(idx).is_some_and(|s| {
                let t = s.trim();
                t == "1" || t.eq_ignore_ascii_case("true")
            })
        };
        let underline = parse_flag(6);
        let strikeout = parse_flag(7);
        let mut letter_spacing = 0.0f32;
        for part in parts.iter().skip(10) {
            let token = part.trim();
            if let Some(val) = token
                .strip_prefix("letterSpacing=")
                .or_else(|| token.strip_prefix("letter-spacing="))
            {
                if let Ok(parsed) = val.trim().parse::<f32>() {
                    if parsed.is_finite() {
                        letter_spacing = parsed.clamp(-8.0, 32.0);
                    }
                }
            }
        }
        if letter_spacing == 0.0 {
            if let Some(raw_ls) = parts.get(16).map(|s| s.trim()) {
                if let Ok(parsed) = raw_ls.parse::<f32>() {
                    if parsed.is_finite() && parsed.abs() <= 32.0 {
                        letter_spacing = parsed;
                    }
                }
            }
        }
        ParsedFontAttributes {
            family: spec.family,
            size: (spec.point_size as f32).clamp(min_size, max_size),
            bold: spec.bold,
            italic: spec.italic,
            underline,
            strikeout,
            letter_spacing,
        }
    } else {
        ParsedFontAttributes {
            family: default_family.to_owned(),
            size: default_size,
            bold: false,
            italic: false,
            underline: false,
            strikeout: false,
            letter_spacing: 0.0,
        }
    }
}

fn setting_font_spec(
    config: &ConfigController,
    group: &str,
    key: &str,
    default_family: &str,
    default_size: f32,
) -> ParsedFontAttributes {
    let raw_font = setting_text(config, group, key, "");
    parse_font_attributes(&raw_font, default_family, default_size, 8.0, 72.0)
}

/// Resolves whether the application UI should use the light theme given a configured
/// theme mode string (`"light"`, `"dark"`, `"system"`) and detected OS color scheme.
pub(crate) fn resolve_theme_mode_is_light(mode: ThemeMode, system: SystemColorScheme) -> bool {
    match mode {
        ThemeMode::Light => true,
        ThemeMode::Dark => false,
        ThemeMode::System => system == SystemColorScheme::Light,
    }
}

/// Resolves the active light/dark theme boolean from `ConfigController` (`interface/theme`).
pub(crate) fn resolve_is_light_theme(config: &ConfigController) -> bool {
    let mode = config.theme_mode();
    let system = match mode {
        ThemeMode::System => memento_os::detect_system_color_scheme(),
        ThemeMode::Light => SystemColorScheme::Light,
        ThemeMode::Dark => SystemColorScheme::Dark,
    };
    resolve_theme_mode_is_light(mode, system)
}

/// Builds a `dic_id -> priority` map from `ConfigController`'s `dictionary/order` setting.
pub(crate) fn dictionary_order_map(config: &ConfigController) -> HashMap<i64, i64> {
    let mut map = HashMap::new();
    if let Some(Value::Ints(order)) =
        config.effective_setting(raw::dictionary::GROUP, raw::dictionary::ORDER)
    {
        map.reserve(order.len());
        for (priority, dic_id) in order.into_iter().enumerate() {
            map.entry(dic_id).or_insert(priority as i64);
        }
    }
    map
}

/// Computes a deterministic hash covering `audio/sources` and `audio/download-timeout-ms`
/// so audio playback caches and popup source menus invalidate cleanly whenever settings change.
pub(crate) fn compute_audio_config_hash(config: &ConfigController) -> u64 {
    let mut hasher = DefaultHasher::new();
    config.audio_download_timeout_ms().hash(&mut hasher);
    let sources = config.audio_source_entries();
    sources.len().hash(&mut hasher);
    for src in &sources {
        src.name.hash(&mut hasher);
        src.url.hash(&mut hasher);
        src.kind.hash(&mut hasher);
        src.md5.hash(&mut hasher);
    }
    hasher.finish()
}

/// Returns the monotonically increasing audio configuration generation counter.
#[cfg(test)]
pub(crate) fn audio_config_generation() -> u64 {
    AUDIO_CONFIG_GENERATION.load(Ordering::Acquire)
}

/// Synchronizes `audio/download-timeout-ms` and `audio/sources` changes from `ConfigController`,
/// updating the active download timeout and invalidating audio worker & popup caches on change.
pub(crate) fn sync_audio_preferences(config: &ConfigController) -> bool {
    let timeout_ms = config.audio_download_timeout_ms();
    let hash = compute_audio_config_hash(config);
    crate::controller::audio::set_download_timeout_ms(timeout_ms);
    let was_initialized = AUDIO_CONFIG_INITIALIZED.swap(true, Ordering::AcqRel);
    let prev_hash = LAST_AUDIO_CONFIG_HASH.swap(hash, Ordering::AcqRel);
    if was_initialized && prev_hash != hash {
        AUDIO_CONFIG_GENERATION.fetch_add(1, Ordering::AcqRel);
        crate::app::anki_actions::invalidate_audio_caches_for_config_change(timeout_ms);
        true
    } else {
        false
    }
}

pub(crate) fn sync_subtitle_overlay_style_from_config(
    app: &MementoApp,
    config: &ConfigController,
) {
    let sub_font = setting_font_spec(
        config,
        raw::interface::GROUP,
        raw::interface::subtitle::FONT,
        "Noto Sans CJK JP",
        24.0,
    );
    let text_color = setting_color(
        config,
        raw::interface::GROUP,
        raw::interface::subtitle::TEXT_COLOR,
        raw::interface::subtitle::TEXT_COLOR_DEFAULT,
    );
    let stroke_color = setting_color(
        config,
        raw::interface::GROUP,
        raw::interface::subtitle::STROKE_COLOR,
        raw::interface::subtitle::STROKE_COLOR_DEFAULT,
    );
    let bg_color = setting_color(
        config,
        raw::interface::GROUP,
        raw::interface::subtitle::BACKGROUND_COLOR,
        raw::interface::subtitle::BACKGROUND_COLOR_DEFAULT,
    );
    let raw_stroke = setting_number(
        config,
        raw::interface::GROUP,
        raw::interface::subtitle::STROKE,
        raw::interface::subtitle::STROKE_DEFAULT,
    );
    let stroke_width = if raw_stroke <= 0.0 {
        0.0
    } else {
        (raw_stroke * 0.1).clamp(0.5, 4.0)
    };
    let sb = app.global::<SubtitleOverlayBridge>();
    sb.set_sub_font_family(sub_font.family.into());
    sb.set_sub_font_bold(sub_font.bold);
    sb.set_sub_font_italic(sub_font.italic);
    sb.set_sub_text_color(text_color);
    sb.set_sub_stroke_color(stroke_color);
    sb.set_sub_stroke_width(stroke_width);
    sb.set_sub_bg_color(bg_color);
    sb.set_subtitle_bg_color(bg_color);
}

pub(crate) fn exact_matcher_enabled(config: &ConfigController) -> bool {
    setting_enabled(
        config,
        raw::search::GROUP,
        raw::search::matcher::EXACT,
        raw::search::matcher::EXACT_DEFAULT,
    )
}

pub(crate) fn deconj_matcher_enabled(config: &ConfigController) -> bool {
    setting_enabled(
        config,
        raw::search::GROUP,
        raw::search::matcher::DECONJ,
        raw::search::matcher::DECONJ_DEFAULT,
    )
}

pub(crate) fn mecab_matcher_enabled(config: &ConfigController) -> bool {
    match config.effective_setting(raw::search::GROUP, raw::search::matcher::MECAB_IPADIC) {
        Some(Value::Bool(b)) => b,
        _ => raw::search::matcher::MECAB_IPADIC_DEFAULT,
    }
}

pub(crate) fn search_text_filters(config: &ConfigController) -> (String, bool, String) {
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

pub(crate) fn apply_dictionary_order(config: &ConfigController, dictionary: &mut DictController) {
    dictionary.set_exact_enabled(exact_matcher_enabled(config));
    dictionary.set_deconj_enabled(deconj_matcher_enabled(config));
    dictionary.set_mecab_enabled(mecab_matcher_enabled(config));
    let (remove_regex, replace_lines, replace_with) = search_text_filters(config);
    dictionary.set_search_text_filters(&remove_regex, replace_lines, &replace_with);
    dictionary.set_text_replacements(config.text_replacement_options());
    if let Some(Value::Ints(order)) =
        config.effective_setting(raw::dictionary::GROUP, raw::dictionary::ORDER)
    {
        dictionary.set_order(&order);
    }
}

/// Release MeCab and cached furigana state only when BOTH subtitle furigana and
/// the MeCab dictionary matcher are disabled. Cancels both background prewarm
/// queues before unloading MeCab so neither can race to reload `libmecab`, and
/// trims OS heap pages only if memory was actually freed.
pub(crate) fn maybe_release_mecab_resources(
    config: &ConfigController,
    furigana_visible: bool,
) -> bool {
    if furigana_visible || mecab_matcher_enabled(config) {
        return false;
    }
    let cleared_cache = crate::controller::subtitles::clear_furigana_cache_and_cancel_prewarm();
    crate::ui::cancel_subtitle_token_prewarm();
    let unloaded_mecab = memento_util::mecab::unload_mecab();
    if cleared_cache || unloaded_mecab {
        crate::app::trim_os_heap();
        true
    } else {
        false
    }
}

pub(crate) fn refresh_player_preferences(app: &MementoApp, config: &ConfigController) {
    let is_light_theme = resolve_is_light_theme(config);
    app.set_is_light_theme(is_light_theme);
    app.set_compact_glossaries(config.compact_glossaries());
    app.set_compact_tags(config.compact_tags());
    app.set_osc_enabled(setting_enabled(
        config,
        raw::behavior::GROUP,
        "osc-enabled",
        true,
    ));
    sync_subtitle_overlay_style_from_config(app, config);
    let _ = sync_audio_preferences(config);

    app.set_hide_mpv_subtitles(setting_enabled(
        config,
        raw::search::GROUP,
        raw::search::HIDE_MPV_SUBS,
        raw::search::HIDE_MPV_SUBS_DEFAULT,
    ));
    app.set_hide_memento_while_playing(setting_enabled(
        config,
        raw::search::GROUP,
        raw::search::HIDE_SUBS,
        raw::search::HIDE_SUBS_DEFAULT,
    ));
    app.set_hover_pause_enabled(setting_enabled(
        config,
        raw::search::GROUP,
        raw::search::PAUSE_ON_HOVER,
        raw::search::PAUSE_ON_HOVER_DEFAULT,
    ));
    app.set_auto_play_audio_enabled(setting_enabled(
        config,
        raw::search::GROUP,
        raw::search::AUTO_PLAY_AUDIO,
        raw::search::AUTO_PLAY_AUDIO_DEFAULT,
    ));
    let (remove_regex, replace_lines, replace_with) = search_text_filters(config);
    app.set_search_remove_regex(remove_regex.into());
    app.set_search_replace_lines(replace_lines);
    app.set_search_replace_with(replace_with.into());
    let furigana_visible = setting_enabled(
        config,
        raw::search::GROUP,
        raw::search::SUBTITLE_FURIGANA,
        raw::search::SUBTITLE_FURIGANA_DEFAULT,
    );
    app.set_furigana_visible(furigana_visible);
    app.set_furigana_enabled(true);
    let _ = maybe_release_mecab_resources(config, furigana_visible);
    store_cached_furigana_ratio(setting_number(
        config,
        raw::interface::GROUP,
        raw::interface::subtitle::FURIGANA_RATIO,
        raw::interface::subtitle::FURIGANA_RATIO_DEFAULT,
    ));
    app.set_sub_ass_native(setting_enabled(
        config,
        raw::interface::GROUP,
        raw::interface::subtitle::ASS_NATIVE,
        raw::interface::subtitle::ASS_NATIVE_DEFAULT,
    ));
    app.set_autofit_enabled(setting_enabled(
        config,
        raw::behavior::GROUP,
        raw::behavior::AUTO_FIT,
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
    app.set_osc_duration_ms(
        setting_number(
            config,
            raw::behavior::GROUP,
            raw::behavior::OSC_DURATION,
            raw::behavior::OSC_DURATION_DEFAULT as f64,
        )
        .round()
        .clamp(50.0, 9999.0) as i32,
    );
    app.set_osc_fade_ms(
        setting_number(
            config,
            raw::behavior::GROUP,
            raw::behavior::OSC_FADE,
            raw::behavior::OSC_FADE_DEFAULT as f64,
        )
        .round()
        .clamp(0.0, 9999.0) as i32,
    );
    app.set_osc_min_move(
        setting_number(
            config,
            raw::behavior::GROUP,
            raw::behavior::OSC_MIN_MOVE,
            raw::behavior::OSC_MIN_MOVE_DEFAULT as f64,
        )
        .round()
        .max(0.0) as i32,
    );
    app.set_osc_mpv_cursor(setting_enabled(
        config,
        raw::behavior::GROUP,
        raw::behavior::OSC_MPV_CURSOR,
        raw::behavior::OSC_MPV_CURSOR_DEFAULT,
    ));
    app.set_subtitle_cursor_show_enabled(setting_enabled(
        config,
        raw::behavior::GROUP,
        raw::behavior::SUBTITLE_CURSOR_SHOW,
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
    let primary_font = setting_font_spec(
        config,
        raw::interface::GROUP,
        raw::interface::subtitle_list::PRIMARY_FONT,
        "Noto Sans CJK JP",
        DEFAULT_SUB_LIST_FONT_SIZE,
    );
    app.set_sub_list_primary_font_family(primary_font.family.into());
    app.set_sub_list_primary_font_size(primary_font.size);
    app.set_sub_list_primary_font_bold(primary_font.bold);
    app.set_sub_list_primary_font_italic(primary_font.italic);
    let secondary_font = setting_font_spec(
        config,
        raw::interface::GROUP,
        raw::interface::subtitle_list::SECONDARY_FONT,
        "Sans Serif",
        DEFAULT_SUB_LIST_FONT_SIZE,
    );
    app.set_sub_list_secondary_font_family(secondary_font.family.into());
    app.set_sub_list_secondary_font_size(secondary_font.size);
    app.set_sub_list_secondary_font_bold(secondary_font.bold);
    app.set_sub_list_secondary_font_italic(secondary_font.italic);
    app.set_popup_configured_width(
        setting_number(
            config,
            raw::interface::GROUP,
            raw::interface::POPUP_WIDTH,
            raw::interface::POPUP_WIDTH_DEFAULT as f64,
        )
        .round()
        .clamp(160.0, 4096.0),
    );
    app.set_popup_configured_height(
        setting_number(
            config,
            raw::interface::GROUP,
            raw::interface::POPUP_HEIGHT,
            raw::interface::POPUP_HEIGHT_DEFAULT as f64,
        )
        .round()
        .clamp(160.0, 4096.0),
    );
    app.set_subtitle_scale(setting_number(
        config,
        raw::interface::GROUP,
        raw::interface::subtitle::SCALE,
        raw::interface::subtitle::SCALE_DEFAULT,
    ));
    app.set_subtitle_offset(setting_number(
        config,
        raw::interface::GROUP,
        raw::interface::subtitle::OFFSET,
        raw::interface::subtitle::OFFSET_DEFAULT,
    ));
    app.set_auto_pause(setting_enabled(
        config,
        raw::behavior::GROUP,
        raw::behavior::SUBTITLE_PAUSE,
        raw::behavior::SUBTITLE_PAUSE_DEFAULT,
    ));
    app.set_anki_enabled(config.anki_enabled());
    app.set_show_anki_checkbox(setting_enabled(
        config,
        raw::search::GROUP,
        raw::search::SHOW_ANKI_GLOSSARY_CHECKBOX,
        raw::search::SHOW_ANKI_GLOSSARY_CHECKBOX_DEFAULT,
    ));
    app.set_anki_profile(config.current_anki_profile().into());
    let profiles: Vec<slint::SharedString> = config
        .anki_profile_names()
        .into_iter()
        .map(Into::into)
        .collect();
    app.set_anki_profiles(Rc::new(VecModel::from(profiles)).into());
    let profile = format!(
        "{}/{}/{}",
        raw::keybind::GROUP,
        raw::keybind::profile::GROUP,
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
    let max_recursion_depth = setting_number(
        config,
        raw::search::GROUP,
        raw::search::MAX_RECURSION_DEPTH,
        raw::search::MAX_RECURSION_DEPTH_DEFAULT as f64,
    )
    .round()
    .clamp(0.0, 10.0) as i32;
    app.global::<PopupBridge>()
        .set_nested_max_depth(max_recursion_depth);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingsTeardownOutcome {
    AlreadyDropped,
    AbortedVisible,
    DeferredActiveJobs,
    TornDown,
}

/// Evaluates whether a hidden `SettingsSession` can be torn down after its idle
/// grace period. Drops `SettingsSession` and trims OS heap pages only when still
/// hidden and no async dictionary/ASR/Anki job or `AnkiHelpWindow` is active.
pub(crate) fn evaluate_idle_settings_teardown(
    settings_session: &Rc<RefCell<Option<SettingsSession>>>,
) -> SettingsTeardownOutcome {
    let decision = {
        let guard = settings_session.borrow();
        let Some(session) = guard.as_ref() else {
            return SettingsTeardownOutcome::AlreadyDropped;
        };
        if session.is_visible() {
            SettingsTeardownOutcome::AbortedVisible
        } else if session.has_active_jobs() {
            SettingsTeardownOutcome::DeferredActiveJobs
        } else {
            SettingsTeardownOutcome::TornDown
        }
    };
    if decision == SettingsTeardownOutcome::TornDown {
        *settings_session.borrow_mut() = None;
        crate::app::trim_os_heap();
    }
    decision
}

fn arm_settings_idle_teardown(
    timer: &Rc<slint::Timer>,
    settings_session: &Rc<RefCell<Option<SettingsSession>>>,
    delay: Duration,
) {
    let weak_session = Rc::downgrade(settings_session);
    let weak_timer = Rc::downgrade(timer);
    timer.start(slint::TimerMode::SingleShot, delay, move || {
        let Some(session_slot) = weak_session.upgrade() else {
            return;
        };
        if evaluate_idle_settings_teardown(&session_slot)
            == SettingsTeardownOutcome::DeferredActiveJobs
        {
            if let Some(timer) = weak_timer.upgrade() {
                arm_settings_idle_teardown(&timer, &session_slot, SETTINGS_IDLE_RETRY_DELAY);
            }
        }
    });
}

/// Holds debounced single-setting persistence timers for rapid `Ctrl+1/2` and `Alt+1/2`
/// subtitle scale/offset adjustments and the Settings idle-teardown timer, ensuring
/// timers stop cleanly and pending edits flush on shutdown.
pub(crate) struct DebouncedSubtitlePrefSaver {
    config: Rc<ConfigController>,
    config_dir: PathBuf,
    scale_timer: Rc<slint::Timer>,
    offset_timer: Rc<slint::Timer>,
    settings_idle_timer: Rc<slint::Timer>,
    pending_scale: Rc<Cell<Option<f32>>>,
    pending_offset: Rc<Cell<Option<f32>>>,
}

impl DebouncedSubtitlePrefSaver {
    pub(crate) fn flush(&self) {
        self.scale_timer.stop();
        self.offset_timer.stop();
        self.settings_idle_timer.stop();
        if let Some(scale) = self.pending_scale.take() {
            if let Err(error) = self.config.persist_single_setting(
                "interface/sub-scale",
                &scale.to_string(),
                &self.config_dir,
                "memento.conf",
            ) {
                eprintln!("Cannot save subtitle size: {error}");
            }
        }
        if let Some(offset) = self.pending_offset.take() {
            if let Err(error) = self.config.persist_single_setting(
                "interface/sub-offset",
                &offset.to_string(),
                &self.config_dir,
                "memento.conf",
            ) {
                eprintln!("Cannot save subtitle position: {error}");
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn install_settings_and_preference_handlers(
    app: &MementoApp,
    config_ctrl: Rc<ConfigController>,
    dict_ctrl: Rc<RefCell<DictController>>,
    dictionary_manager: Option<Rc<DictionaryManager>>,
    dictionary_database_path: PathBuf,
    config_dir: PathBuf,
    anki_json_path: PathBuf,
    ocr_available: bool,
    asr_session: Rc<RefCell<AsrController>>,
    sub_ctrl: Rc<RefCell<SubtitleController>>,
    sub_model: Rc<VecModel<SubEntry>>,
    asr_restart_pending: Rc<Cell<bool>>,
    mpv: Rc<RefCell<Option<MpvInstance>>>,
    coordinator: Rc<RefCell<Option<LivePlayerCoordinator>>>,
    settings_session: Rc<RefCell<Option<SettingsSession>>>,
) -> DebouncedSubtitlePrefSaver {
    // ── Open Options (lazy creation, 75 s idle teardown, fingerprint-gated reopen) ─
    let settings_idle_timer = Rc::new(slint::Timer::default());
    let settings_on_open = settings_session.clone();
    let open_idle_timer = settings_idle_timer.clone();
    let open_config = config_ctrl.clone();
    let open_dict = dict_ctrl;
    let open_manager = dictionary_manager;
    let open_dictionary_path = dictionary_database_path;
    let open_cache = config_dir.clone();
    let open_anki = anki_json_path.clone();
    let open_app = app.as_weak();
    let open_asr_session = asr_session;
    let open_subtitles = sub_ctrl;
    let open_subtitle_model = sub_model;
    let open_asr_restart = asr_restart_pending;
    let open_settings_mpv = mpv.clone();
    let open_settings_coord = coordinator;
    app.on_open_options(move || {
        let Some(app) = open_app.upgrade() else {
            return;
        };
        // Reopening within the 75 s idle grace window cancels pending teardown immediately.
        open_idle_timer.stop();

        let newly_created = settings_on_open.borrow().is_none();
        if newly_created {
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
                Ok(session) => {
                    let hide_session = Rc::downgrade(&settings_on_open);
                    let hide_timer = Rc::downgrade(&open_idle_timer);
                    session.set_on_hidden(move || {
                        if let (Some(slot), Some(timer)) =
                            (hide_session.upgrade(), hide_timer.upgrade())
                        {
                            arm_settings_idle_teardown(
                                &timer,
                                &slot,
                                SETTINGS_IDLE_TEARDOWN_DELAY,
                            );
                        }
                    });
                    *settings_on_open.borrow_mut() = Some(session);
                }
                Err(error) => {
                    app.set_status_message(format!("Could not open preferences: {error}").into());
                    return;
                }
            }
        }
        let settings = {
            let guard = settings_on_open.borrow();
            let session = guard.as_ref().unwrap();
            session.set_visible(true);
            session.window.set_showing_audio_help(false);
            session.window.set_connection_popup_open(false);
            // `create_settings_session` already populates settings, custom CSS, and dictionary rows.
            // On reopen, refresh only the active category with fingerprint-gated CSS/SQLite checks.
            if !newly_created {
                session.refresh_on_reopen();
            }
            session.window.clone_strong()
        };
        if let Err(error) = settings.show() {
            if let Some(session) = settings_on_open.borrow().as_ref() {
                session.set_visible(false);
            }
            app.set_status_message(format!("Could not open preferences: {error}").into());
            return;
        }
        settings.window().request_redraw();
        app.window().request_redraw();
    });

    // ── Quick Anki profile switcher ─────────────────────────────────────────
    let selected_profile_app = app.as_weak();
    let selected_profile_config = config_ctrl.clone();
    let selected_profile_path = anki_json_path;
    let selected_profile_cache = config_dir.clone();
    app.on_select_anki_profile(move |name| {
        let Some(app) = selected_profile_app.upgrade() else {
            return;
        };
        let result = selected_profile_config
            .edit_setting("anki/profile", &name)
            .and_then(|()| {
                selected_profile_config.apply_settings(
                    0,
                    &selected_profile_cache,
                    "memento.conf",
                    &selected_profile_path,
                )
            });
        match result {
            Ok(()) => refresh_player_preferences(&app, &selected_profile_config),
            Err(error) => {
                app.set_status_message(format!("Could not select Anki profile: {error}").into())
            }
        }
    });

    // ── Subtitle auto-pause, debounced scale/offset, visibility, furigana ───
    let auto_app = app.as_weak();
    let auto_config = config_ctrl.clone();
    let auto_cache = config_dir.clone();
    app.on_auto_pause_changed(move |enabled| {
        let result = auto_config.persist_single_setting(
            "behavior/subtitle-pause",
            if enabled { "true" } else { "false" },
            &auto_cache,
            "memento.conf",
        );
        match result {
            Ok(()) => {
                if let Some(app) = auto_app.upgrade() {
                    app.set_auto_pause(enabled);
                }
            }
            Err(error) => eprintln!("Cannot save subtitle auto-pause: {error}"),
        }
    });

    const SUBTITLE_TWEAK_DEBOUNCE: Duration = Duration::from_millis(300);
    let scale_timer = Rc::new(slint::Timer::default());
    let pending_scale = Rc::new(Cell::new(None::<f32>));
    let scale_app = app.as_weak();
    let scale_config = config_ctrl.clone();
    let scale_cache = config_dir.clone();
    let scale_mpv = mpv.clone();
    let scale_timer_cb = scale_timer.clone();
    let scale_pending_cb = pending_scale.clone();
    app.on_adjust_subtitle_scale(move |delta| {
        let Some(app) = scale_app.upgrade() else {
            return;
        };
        if app
            .global::<SubtitleOverlayBridge>()
            .get_ass_native_active()
        {
            if let Some(player) = scale_mpv.borrow().as_ref() {
                let cur = player.sub_scale();
                let raw_next = (cur + f64::from(delta) * 20.0).clamp(0.2, 3.0);
                let next = if (raw_next - 1.0).abs() < 0.025 {
                    1.0
                } else {
                    raw_next
                };
                if let Err(error) = player.set_sub_scale(next) {
                    eprintln!("Cannot adjust ASS subtitle scale: {error}");
                } else {
                    app.set_status_message(
                        format!("ASS subtitle scale: {}%", (next * 100.0).round() as i32).into(),
                    );
                }
            }
            return;
        }
        let new_scale = (app.get_subtitle_scale() + delta).clamp(0.001, 1.0);
        app.set_subtitle_scale(new_scale);
        scale_pending_cb.set(Some(new_scale));
        let cfg = scale_config.clone();
        let dir = scale_cache.clone();
        let pending = scale_pending_cb.clone();
        scale_timer_cb.start(
            slint::TimerMode::SingleShot,
            SUBTITLE_TWEAK_DEBOUNCE,
            move || {
                if let Some(value) = pending.take() {
                    if let Err(error) = cfg.persist_single_setting(
                        "interface/sub-scale",
                        &value.to_string(),
                        &dir,
                        "memento.conf",
                    ) {
                        eprintln!("Cannot save subtitle size: {error}");
                    }
                }
            },
        );
    });

    let offset_timer = Rc::new(slint::Timer::default());
    let pending_offset = Rc::new(Cell::new(None::<f32>));
    let offset_app = app.as_weak();
    let offset_config = config_ctrl.clone();
    let offset_cache = config_dir.clone();
    let offset_mpv = mpv;
    let offset_timer_cb = offset_timer.clone();
    let offset_pending_cb = pending_offset.clone();
    app.on_adjust_subtitle_offset(move |delta| {
        let Some(app) = offset_app.upgrade() else {
            return;
        };
        if app
            .global::<SubtitleOverlayBridge>()
            .get_ass_native_active()
        {
            if let Some(player) = offset_mpv.borrow().as_ref() {
                let cur = player.sub_pos();
                let next = (cur - f64::from(delta) * 200.0).clamp(0.0, 150.0);
                if let Err(error) = player.set_sub_pos(next) {
                    eprintln!("Cannot adjust ASS subtitle position: {error}");
                } else {
                    app.set_status_message(
                        format!("ASS subtitle position: {}%", next.round() as i32).into(),
                    );
                }
            }
            return;
        }
        let new_offset = (app.get_subtitle_offset() + delta).clamp(0.0, 1.0);
        app.set_subtitle_offset(new_offset);
        offset_pending_cb.set(Some(new_offset));
        let cfg = offset_config.clone();
        let dir = offset_cache.clone();
        let pending = offset_pending_cb.clone();
        offset_timer_cb.start(
            slint::TimerMode::SingleShot,
            SUBTITLE_TWEAK_DEBOUNCE,
            move || {
                if let Some(value) = pending.take() {
                    if let Err(error) = cfg.persist_single_setting(
                        "interface/sub-offset",
                        &value.to_string(),
                        &dir,
                        "memento.conf",
                    ) {
                        eprintln!("Cannot save subtitle position: {error}");
                    }
                }
            },
        );
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
    app.on_furigana_visible_changed(move |visible| {
        let Some(app) = furigana_app.upgrade() else {
            return;
        };
        let result = furigana_config.persist_single_setting(
            "search/subtitle-furigana",
            if visible { "true" } else { "false" },
            &furigana_cache,
            "memento.conf",
        );
        match result {
            Ok(()) => refresh_player_preferences(&app, &furigana_config),
            Err(error) => {
                app.set_status_message(format!("Could not change furigana: {error}").into())
            }
        }
    });

    // ── Subtitle drawer preference toggles ──────────────────────────────────
    let ts_app = app.as_weak();
    let ts_config = config_ctrl.clone();
    let ts_cache = config_dir.clone();
    let ts_settings = settings_session.clone();
    app.on_sub_list_show_timestamps_toggled(move |show| {
        let Some(app) = ts_app.upgrade() else {
            return;
        };
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
        let Some(app) = ws_app.upgrade() else {
            return;
        };
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
    let seek_settings = settings_session;
    app.on_subtitle_search_auto_seek_changed(move |enabled| {
        let Some(app) = seek_app.upgrade() else {
            return;
        };
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
                crate::app::subtitle_drawer::auto_seek_to_matched_row(&app, focused as usize);
            }
        }
    });

    DebouncedSubtitlePrefSaver {
        config: config_ctrl,
        config_dir,
        scale_timer,
        offset_timer,
        settings_idle_timer,
        pending_scale,
        pending_offset,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::subtitle_drawer::{
        estimate_drawer_row_height, install_subtitle_selection, refresh_subtitle_list,
    };
    use crate::controller::dict::SearchHit;
    use crate::controller::subtitles::{create_cue, SubtitleController};
    use slint::platform::{PointerEventButton, WindowEvent};

    #[test]
    fn recent_titles_cache_is_bounded() {
        let mut cache = BoundedRecentTitles::default();
        for i in 0..(MAX_RECENT_TITLES + 20) {
            cache.insert(format!("/vid/{i}.mkv"), format!("Title {i}"));
        }
        assert_eq!(cache.len(), MAX_RECENT_TITLES);
        assert!(cache.get("/vid/0.mkv").is_none());
        assert_eq!(
            cache.get(&format!("/vid/{}.mkv", MAX_RECENT_TITLES + 19)),
            Some(format!("Title {}", MAX_RECENT_TITLES + 19).as_str())
        );
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
        assert_eq!(
            dict.mecab_enabled(),
            raw::search::matcher::MECAB_IPADIC_DEFAULT
        );

        let key = format!(
            "{}/{}",
            raw::search::GROUP,
            raw::search::matcher::MECAB_IPADIC
        );
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
        let mut noun = SearchHit::new("猫", "ねこ", "cat", "n", 10, false);
        noun.rules = "n".to_owned();
        let mut verb = SearchHit::new("食べる", "たべる", "to eat", "v1", 10, false);
        verb.rules = "v1".to_owned();
        dict.add_entries([noun, verb]);

        cfg.edit_setting("search/ipadic-matcher", "false").unwrap();
        cfg.edit_setting("search/exact-matcher", "false").unwrap();
        cfg.edit_setting("search/deconj-matcher", "true").unwrap();
        cfg.edit_setting("search/remove-regex", "（[^）]*）")
            .unwrap();
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
            dict.search("（笑）食べ\nた")
                .first()
                .map(|h| h.expression.as_str()),
            Some("食べる")
        );

        cfg.edit_setting("search/exact-matcher", "true").unwrap();
        cfg.edit_setting("search/deconj-matcher", "false").unwrap();
        apply_dictionary_order(&cfg, &mut dict);
        assert!(dict.exact_enabled());
        assert!(!dict.deconj_enabled());
        assert_eq!(
            dict.search("（笑）猫")
                .first()
                .map(|h| h.expression.as_str()),
            Some("猫")
        );
        assert!(dict.search("食べた").is_empty());
    }

    #[test]
    fn subtitle_list_timestamps_toggle_ignore_whitespace_auto_seek_and_styling_binding() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::TempDir::new().unwrap();
        let cfg = Rc::new(ConfigController::new());
        cfg.edit_setting("interface/sub-list-timestamps", "true")
            .unwrap();
        cfg.edit_setting("subtitle-list/ignore-whitespace", "false")
            .unwrap();
        cfg.edit_setting("subtitle-list/auto-seek", "true").unwrap();
        cfg.edit_setting("interface/sub-list-background-color", "#FF112233")
            .unwrap();
        cfg.edit_setting("interface/sub-list-text-color", "#FFCCDDEE")
            .unwrap();
        cfg.edit_setting("interface/sub-list-hover-color", "#FF223344")
            .unwrap();
        cfg.edit_setting("interface/sub-list-selected-color", "#FF334455")
            .unwrap();
        cfg.edit_setting("interface/sub-list-selected-hover-color", "#FF445566")
            .unwrap();
        cfg.edit_setting(
            "interface/sub-list-primary-font",
            "Noto Sans CJK JP,18,-1,5,75,1,0,0,0,0",
        )
        .unwrap();
        cfg.edit_setting(
            "interface/sub-list-secondary-font",
            "Monospace,14,-1,5,50,0,0,0,0,0",
        )
        .unwrap();

        let app = MementoApp::new().unwrap();
        app.window().set_size(slint::LogicalSize::new(1280., 720.));
        app.set_sub_drawer_open(true);
        refresh_player_preferences(&app, &cfg);

        assert!(app.get_sub_list_show_timestamps());
        assert!(!app.get_subtitle_search_ignore_whitespace());
        assert!(app.get_subtitle_search_auto_seek());
        assert_eq!(
            app.get_sub_list_bg_color(),
            slint::Color::from_argb_u8(255, 0x11, 0x22, 0x33)
        );
        assert_eq!(
            app.get_sub_list_text_color(),
            slint::Color::from_argb_u8(255, 0xCC, 0xDD, 0xEE)
        );
        assert_eq!(
            app.get_sub_list_hover_color(),
            slint::Color::from_argb_u8(255, 0x22, 0x33, 0x44)
        );
        assert_eq!(
            app.get_sub_list_selected_color(),
            slint::Color::from_argb_u8(255, 0x33, 0x44, 0x55)
        );
        assert_eq!(
            app.get_sub_list_selected_hover_color(),
            slint::Color::from_argb_u8(255, 0x44, 0x55, 0x66)
        );
        assert_eq!(
            app.get_sub_list_primary_font_family().as_str(),
            "Noto Sans CJK JP"
        );
        assert_eq!(app.get_sub_list_primary_font_size(), 18.0);
        assert!(app.get_sub_list_primary_font_bold());
        assert!(app.get_sub_list_primary_font_italic());
        assert_eq!(
            app.get_sub_list_secondary_font_family().as_str(),
            "Monospace"
        );
        assert_eq!(app.get_sub_list_secondary_font_size(), 14.0);
        assert!(!app.get_sub_list_secondary_font_bold());
        assert!(!app.get_sub_list_secondary_font_italic());

        let ts_cfg = cfg.clone();
        let ts_dir = dir.path().to_path_buf();
        app.on_sub_list_show_timestamps_toggled(move |show| {
            let key = format!(
                "{}/{}",
                raw::interface::GROUP,
                raw::interface::subtitle_list::TIMESTAMPS
            );
            ts_cfg
                .persist_single_setting(
                    &key,
                    if show { "true" } else { "false" },
                    &ts_dir,
                    "memento.conf",
                )
                .unwrap();
        });
        app.show().unwrap();
        // Drawer right edge = 1280; right pad = 10, Close button = 28, spacing = 6,
        // Time pill = 58px -> x in [1178, 1236], menu bar 30 + top pad 10 + 14 = y 54.
        let pos = slint::LogicalPosition::new(1207.0, 54.0);
        app.window().dispatch_event(WindowEvent::PointerPressed {
            position: pos,
            button: PointerEventButton::Left,
        });
        app.window().dispatch_event(WindowEvent::PointerReleased {
            position: pos,
            button: PointerEventButton::Left,
        });
        assert!(
            !app.get_sub_list_show_timestamps(),
            "clicking Time pill must toggle timestamps off"
        );
        let reloaded = ConfigController::load(dir.path(), "memento.conf").unwrap();
        assert_eq!(
            reloaded.effective_setting(
                raw::interface::GROUP,
                raw::interface::subtitle_list::TIMESTAMPS
            ),
            Some(Value::Bool(false))
        );

        let subtitles = Rc::new(RefCell::new(SubtitleController::new()));
        subtitles.borrow_mut().load_cues(vec![
            create_cue("Alpha one", 12.0, 14.0),
            create_cue("Beta two", 25.0, 27.0),
            create_cue("Alpha   three", 41.0, 43.0),
        ]);
        let rows = Rc::new(VecModel::<SubEntry>::default());
        app.set_subtitle_entries(rows.clone().into());
        refresh_subtitle_list(&app, &rows, &subtitles.borrow());
        install_subtitle_selection(&app, subtitles.clone(), rows.clone());

        let sought = Rc::new(RefCell::new(Vec::<f32>::new()));
        let sought_sink = sought.clone();
        app.on_seek_to(move |sec| sought_sink.borrow_mut().push(sec));

        app.invoke_search_subtitles("Alphaone".into());
        assert_eq!(app.get_subtitle_search_total_matches(), 0);
        assert!(sought.borrow().is_empty());

        app.set_subtitle_search_ignore_whitespace(true);
        app.invoke_search_subtitles("Alpha".into());
        assert_eq!(app.get_subtitle_search_total_matches(), 2);
        assert_eq!(sought.borrow().as_slice(), &[12.0]);
        app.invoke_next_subtitle_search_match();
        assert_eq!(sought.borrow().as_slice(), &[12.0, 41.0]);
        app.invoke_prev_subtitle_search_match();
        assert_eq!(sought.borrow().as_slice(), &[12.0, 41.0, 12.0]);

        app.set_subtitle_search_auto_seek(false);
        app.invoke_next_subtitle_search_match();
        assert_eq!(sought.borrow().as_slice(), &[12.0, 41.0, 12.0]);
        app.hide().unwrap();
    }

    #[test]
    fn max_recursion_depth_syncs_to_popup_bridge_and_compact_row_estimate_matches_24px() {
        i_slint_backend_testing::init_no_event_loop();
        let app = MementoApp::new().unwrap();
        let cfg = ConfigController::new();

        refresh_player_preferences(&app, &cfg);
        assert_eq!(app.global::<PopupBridge>().get_nested_max_depth(), 4);

        cfg.edit_setting("search/max-recursion-depth", "7").unwrap();
        refresh_player_preferences(&app, &cfg);
        assert_eq!(app.global::<PopupBridge>().get_nested_max_depth(), 7);

        cfg.edit_setting("search/max-recursion-depth", "0").unwrap();
        refresh_player_preferences(&app, &cfg);
        assert_eq!(app.global::<PopupBridge>().get_nested_max_depth(), 0);

        cfg.edit_setting("search/max-recursion-depth", "99")
            .unwrap();
        refresh_player_preferences(&app, &cfg);
        assert_eq!(app.global::<PopupBridge>().get_nested_max_depth(), 10);

        assert_eq!(estimate_drawer_row_height("短い字幕", 260.0, 13.0), 24.0);
        assert!(estimate_drawer_row_height("line1\nline2", 260.0, 13.0) > 36.0);
    }

    #[test]
    fn theme_compact_osc_subtitle_style_and_relaxed_popup_clamps_sync() {
        i_slint_backend_testing::init_no_event_loop();
        let app = MementoApp::new().unwrap();
        let cfg = ConfigController::new();

        assert!(resolve_theme_mode_is_light(
            ThemeMode::Light,
            SystemColorScheme::Dark
        ));
        assert!(!resolve_theme_mode_is_light(
            ThemeMode::Dark,
            SystemColorScheme::Light
        ));
        assert!(resolve_theme_mode_is_light(
            ThemeMode::System,
            SystemColorScheme::Light
        ));
        assert!(!resolve_theme_mode_is_light(
            ThemeMode::System,
            SystemColorScheme::Dark
        ));

        cfg.edit_setting("interface/theme", "light").unwrap();
        cfg.edit_setting("interface/compact-glossaries", "true")
            .unwrap();
        cfg.edit_setting("interface/compact-tags", "true").unwrap();
        cfg.edit_setting("interface/sub-popup-width", "1800")
            .unwrap();
        cfg.edit_setting("interface/sub-popup-height", "1200")
            .unwrap();
        cfg.edit_setting("interface/sub-font", "Noto Serif JP,24,-1,5,75,1,0,0,0,0")
            .unwrap();
        cfg.edit_setting("interface/sub-text-color", "#FFEECCAA")
            .unwrap();
        cfg.edit_setting("interface/sub-stroke-color", "#FF112233")
            .unwrap();
        cfg.edit_setting("interface/sub-bg-color", "#80001122")
            .unwrap();
        cfg.edit_setting("interface/sub-stroke", "25.0").unwrap();

        refresh_player_preferences(&app, &cfg);

        assert!(app.get_is_light_theme());
        assert!(app.get_osc_enabled());
        let pb = app.global::<PopupBridge>();
        assert!(pb.get_compact_glossaries());
        assert!(pb.get_compact_tags());
        assert_eq!(app.get_popup_configured_width(), 1800.0);
        assert_eq!(app.get_popup_configured_height(), 1200.0);

        let sub = app.global::<SubtitleOverlayBridge>();
        assert_eq!(sub.get_sub_font_family().as_str(), "Noto Serif JP");
        assert!(sub.get_sub_font_bold());
        assert!(sub.get_sub_font_italic());
        assert_eq!(
            sub.get_sub_text_color(),
            slint::Color::from_argb_u8(0xFF, 0xEE, 0xCC, 0xAA)
        );
        assert_eq!(
            sub.get_sub_stroke_color(),
            slint::Color::from_argb_u8(0xFF, 0x11, 0x22, 0x33)
        );
        assert_eq!(
            sub.get_sub_bg_color(),
            slint::Color::from_argb_u8(0x80, 0x00, 0x11, 0x22)
        );
        assert!((sub.get_sub_stroke_width() - 2.5).abs() < 1e-4);

        // Relaxed clamps allow 160..=4096 (clamping out-of-bounds values cleanly).
        cfg.edit_setting("interface/sub-popup-width", "100")
            .unwrap();
        cfg.edit_setting("interface/sub-popup-height", "9000")
            .unwrap();
        cfg.edit_setting("interface/theme", "dark").unwrap();
        refresh_player_preferences(&app, &cfg);
        assert!(!app.get_is_light_theme());
        assert_eq!(app.get_popup_configured_width(), 160.0);
        assert_eq!(app.get_popup_configured_height(), 4096.0);
    }

    #[test]
    fn apply_dictionary_order_propagates_text_replacements_and_font_attrs_parse() {
        let cfg = ConfigController::new();
        let mut dict = DictController::new();
        let hit = SearchHit::new("食べる", "たべる", "to eat", "v1", 10, false);
        dict.add_entries([hit]);

        cfg.edit_setting(
            "search/text-replacements",
            r#"[{"pattern":"喰","replacement":"食","ignore_case":false,"enabled":true}]"#,
        )
        .unwrap();
        cfg.edit_setting("search/text-replacements-search-original", "true")
            .unwrap();
        apply_dictionary_order(&cfg, &mut dict);

        let hits = dict.search("喰べる");
        assert_eq!(
            hits.first().map(|h| h.expression.as_str()),
            Some("食べる")
        );

        let attrs = parse_font_attributes(
            "Noto Sans,16,-1,5,75,1,1,1,0,0,letterSpacing=1.5",
            "Sans Serif",
            16.0,
            8.0,
            200.0,
        );
        assert!(attrs.underline);
        assert!(attrs.strikeout);
        assert!((attrs.letter_spacing - 1.5).abs() < 1e-4);

        #[cfg(target_os = "macos")]
        assert_eq!(DEFAULT_SUB_LIST_FONT_SIZE, 20.0);
        #[cfg(not(target_os = "macos"))]
        assert_eq!(DEFAULT_SUB_LIST_FONT_SIZE, 16.0);
    }

    #[test]
    fn sync_audio_preferences_tracks_timeout_and_source_changes() {
        let cfg = ConfigController::new();
        sync_audio_preferences(&cfg);
        let gen0 = audio_config_generation();

        cfg.edit_setting("audio/download-timeout-ms", "8500")
            .unwrap();
        sync_audio_preferences(&cfg);
        let gen1 = audio_config_generation();
        assert!(gen1 > gen0);
        assert_eq!(crate::controller::audio::download_timeout_ms(), 8_500);

        // Unchanged config does not re-invalidate.
        sync_audio_preferences(&cfg);
        assert_eq!(audio_config_generation(), gen1);
    }

    #[test]
    fn settings_idle_teardown_reopen_cancels_active_jobs_block_and_idle_drops_cleanly() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let app = MementoApp::new().unwrap();
        let cfg = Rc::new(ConfigController::new());
        let dict_ctrl = Rc::new(RefCell::new(DictController::new()));
        let asr_session = Rc::new(RefCell::new(AsrController::new()));
        let sub_ctrl = Rc::new(RefCell::new(SubtitleController::new()));
        let sub_model = Rc::new(VecModel::<SubEntry>::default());
        let asr_restart = Rc::new(Cell::new(false));
        let mpv = Rc::new(RefCell::new(None));
        let coord = Rc::new(RefCell::new(None));
        let settings_session: Rc<RefCell<Option<SettingsSession>>> = Rc::new(RefCell::new(None));

        let pref_saver = install_settings_and_preference_handlers(
            &app,
            cfg,
            dict_ctrl,
            None,
            dir.path().join("dictionaries.sqlite"),
            dir.path().to_path_buf(),
            dir.path().join("anki_connect.json"),
            false,
            asr_session,
            sub_ctrl,
            sub_model,
            asr_restart,
            mpv,
            coord,
            settings_session.clone(),
        );

        assert_eq!(SETTINGS_IDLE_TEARDOWN_DELAY, Duration::from_secs(75));
        assert_eq!(SETTINGS_IDLE_RETRY_DELAY, Duration::from_secs(45));
        assert_eq!(
            evaluate_idle_settings_teardown(&settings_session),
            SettingsTeardownOutcome::AlreadyDropped
        );

        // Open Settings -> lazily constructs session, marks visible, timer stopped.
        app.invoke_open_options();
        assert!(settings_session.borrow().is_some());
        assert!(settings_session.borrow().as_ref().unwrap().is_visible());
        assert!(!pref_saver.settings_idle_timer.running());
        let first_win_weak = settings_session.borrow().as_ref().unwrap().window.as_weak();

        // Hide Settings -> marks hidden and arms the 75 s single-shot timer.
        settings_session.borrow().as_ref().unwrap().window.invoke_close();
        assert!(!settings_session.borrow().as_ref().unwrap().is_visible());
        assert!(
            pref_saver.settings_idle_timer.running(),
            "closing SettingsWindow must arm the 75 s idle teardown timer"
        );

        // (a) Reopening within the grace window cancels the timer and reuses the same window.
        app.invoke_open_options();
        assert!(
            !pref_saver.settings_idle_timer.running(),
            "reopening SettingsWindow must cancel pending idle teardown timer"
        );
        assert!(settings_session.borrow().as_ref().unwrap().is_visible());
        assert_eq!(
            evaluate_idle_settings_teardown(&settings_session),
            SettingsTeardownOutcome::AbortedVisible
        );
        assert!(first_win_weak.upgrade().is_some());

        // Hide again so teardown checks run against hidden state.
        settings_session.borrow().as_ref().unwrap().window.invoke_close();
        assert!(!settings_session.borrow().as_ref().unwrap().is_visible());
        assert!(pref_saver.settings_idle_timer.running());

        // (b) Active dict / queued drop / ASR / Anki / Help window each block teardown.
        settings_session
            .borrow()
            .as_ref()
            .unwrap()
            .window
            .set_dictionary_busy(true);
        assert_eq!(
            evaluate_idle_settings_teardown(&settings_session),
            SettingsTeardownOutcome::DeferredActiveJobs
        );
        assert!(settings_session.borrow().is_some());
        settings_session
            .borrow()
            .as_ref()
            .unwrap()
            .window
            .set_dictionary_busy(false);

        settings_session
            .borrow()
            .as_ref()
            .unwrap()
            .push_queued_dict_import_for_test(dir.path().join("queued.zip"));
        assert_eq!(
            evaluate_idle_settings_teardown(&settings_session),
            SettingsTeardownOutcome::DeferredActiveJobs
        );
        settings_session
            .borrow()
            .as_ref()
            .unwrap()
            .clear_queued_dict_imports_for_test();

        settings_session
            .borrow()
            .as_ref()
            .unwrap()
            .window
            .set_asr_download_active(true);
        assert_eq!(
            evaluate_idle_settings_teardown(&settings_session),
            SettingsTeardownOutcome::DeferredActiveJobs
        );
        settings_session
            .borrow()
            .as_ref()
            .unwrap()
            .window
            .set_asr_download_active(false);

        settings_session
            .borrow()
            .as_ref()
            .unwrap()
            .set_anki_auto_fetch_in_flight_for_test(true);
        assert_eq!(
            evaluate_idle_settings_teardown(&settings_session),
            SettingsTeardownOutcome::DeferredActiveJobs
        );
        settings_session
            .borrow()
            .as_ref()
            .unwrap()
            .set_anki_auto_fetch_in_flight_for_test(false);

        settings_session
            .borrow()
            .as_ref()
            .unwrap()
            .window
            .invoke_anki_help_requested();
        let help_weak = settings_session
            .borrow()
            .as_ref()
            .unwrap()
            .help_window_weak_for_test()
            .expect("help window created");
        assert_eq!(
            evaluate_idle_settings_teardown(&settings_session),
            SettingsTeardownOutcome::DeferredActiveJobs
        );
        settings_session
            .borrow()
            .as_ref()
            .unwrap()
            .close_help_window_for_test();

        // (c) Hidden idle session with no active jobs tears down cleanly and drops native handles.
        assert_eq!(
            evaluate_idle_settings_teardown(&settings_session),
            SettingsTeardownOutcome::TornDown
        );
        assert!(settings_session.borrow().is_none());
        assert!(
            first_win_weak.upgrade().is_none(),
            "SettingsWindow must be dropped on idle teardown"
        );
        assert!(
            help_weak.upgrade().is_none(),
            "AnkiHelpWindow must be dropped on idle teardown"
        );

        // Reopening after teardown cleanly re-creates a fresh session.
        app.invoke_open_options();
        assert!(settings_session.borrow().is_some());
        assert!(settings_session.borrow().as_ref().unwrap().is_visible());

        settings_session.borrow().as_ref().unwrap().window.invoke_close();
        assert!(pref_saver.settings_idle_timer.running());
        pref_saver.flush();
        assert!(!pref_saver.settings_idle_timer.running());
    }

    #[test]
    fn mecab_unload_and_prewarm_cancel_only_when_both_furigana_and_matcher_disabled() {
        use crate::controller::subtitles::furigana::{
            cached_subtitle_furigana_parts, insert_cached_furigana_for_test, prewarm_epoch_for_test,
        };

        i_slint_backend_testing::init_no_event_loop();
        let app = MementoApp::new().unwrap();
        let cfg = ConfigController::new();
        let sample_key = "memento_pref_mecab_guard_line_7e6d5c";

        // 1. Furigana enabled + matcher enabled -> keep cache & prewarm untouched.
        cfg.edit_setting("search/subtitle-furigana", "true")
            .unwrap();
        cfg.edit_setting("search/ipadic-matcher", "true").unwrap();
        insert_cached_furigana_for_test(sample_key, vec![("漢字".into(), "かんじ".into())]);
        let furigana_epoch_0 = prewarm_epoch_for_test();
        let token_epoch_0 = crate::ui::subtitle_token_prewarm_epoch_for_test();
        refresh_player_preferences(&app, &cfg);
        assert!(cached_subtitle_furigana_parts(sample_key).is_some());
        assert_eq!(prewarm_epoch_for_test(), furigana_epoch_0);
        assert_eq!(
            crate::ui::subtitle_token_prewarm_epoch_for_test(),
            token_epoch_0
        );

        // 2. Furigana enabled + matcher disabled -> still keep cache & MeCab.
        cfg.edit_setting("search/ipadic-matcher", "false").unwrap();
        refresh_player_preferences(&app, &cfg);
        assert!(cached_subtitle_furigana_parts(sample_key).is_some());
        assert_eq!(prewarm_epoch_for_test(), furigana_epoch_0);
        assert_eq!(
            crate::ui::subtitle_token_prewarm_epoch_for_test(),
            token_epoch_0
        );

        // 3. Furigana disabled + matcher enabled -> still keep MeCab & cache.
        cfg.edit_setting("search/subtitle-furigana", "false")
            .unwrap();
        cfg.edit_setting("search/ipadic-matcher", "true").unwrap();
        refresh_player_preferences(&app, &cfg);
        assert!(cached_subtitle_furigana_parts(sample_key).is_some());
        assert_eq!(prewarm_epoch_for_test(), furigana_epoch_0);
        assert_eq!(
            crate::ui::subtitle_token_prewarm_epoch_for_test(),
            token_epoch_0
        );

        // 4. Both disabled -> cancel BOTH prewarm queues, clear furigana cache, unload MeCab.
        cfg.edit_setting("search/ipadic-matcher", "false").unwrap();
        refresh_player_preferences(&app, &cfg);
        assert!(
            prewarm_epoch_for_test() > furigana_epoch_0,
            "disabling both features must cancel furigana prewarm"
        );
        assert!(
            crate::ui::subtitle_token_prewarm_epoch_for_test() > token_epoch_0,
            "disabling both features must cancel subtitle token prewarm"
        );
        assert_eq!(
            cached_subtitle_furigana_parts(sample_key),
            None,
            "disabling both features must clear the furigana LRU cache"
        );
        assert!(
            !memento_util::mecab::is_mecab_loaded(),
            "MeCab must be unloaded when both furigana and matcher are disabled"
        );

        // 5. Repeated call when already unloaded & cache empty reports no memory freed (skips trim).
        assert!(!maybe_release_mecab_resources(&cfg, false));
    }
}

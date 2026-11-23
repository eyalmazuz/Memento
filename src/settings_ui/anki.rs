//! Anki settings callbacks: theme-synced help window, note presets, auto-fetch
//! decks & models on open, `ex-glos` excluded dictionary checklist, marker &
//! argument autocomplete popup, guarded connection testing, and timer-free async
//! model field loading via `Weak::upgrade_in_event_loop`.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use subomi_os::SystemColorScheme;
use slint::winit_030::winit::event::WindowEvent;
use slint::winit_030::winit::window::Theme as WinitTheme;
use slint::winit_030::{EventResult, WinitWindowAccessor};
use slint::{ComponentHandle, Model as _, VecModel};

use crate::anki_help;
use crate::controller::anki::AnkiController;
use crate::controller::config::ConfigController;
use crate::controller::dictmanager::DictionaryManager;
use crate::ui::{AnkiAutocompleteItem, AnkiExcludedDictRow};
use crate::{AnkiHelpWindow, SettingsWindow};

use super::dictionaries::ordered_dictionaries;
use super::{refresh_category_settings, sync_after_edit, SettingsCategory, SettingsContext};

struct PendingAnkiFields {
    section: String,
    profile_name: String,
    model: String,
    result: Result<Vec<String>, String>,
}

// ── Anki Marker & Argument Autocomplete Catalog ─────────────────────────────

struct MarkerSpec {
    name: &'static str,
    detail: &'static str,
}

struct ArgSpec {
    name: &'static str,
    detail: &'static str,
    values: &'static [&'static str],
}

const TERM_MARKERS: &[MarkerSpec] = &[
    MarkerSpec { name: "expression", detail: "Expression in kanji (or kana)" },
    MarkerSpec { name: "reading", detail: "Kana reading (empty if kana-only)" },
    MarkerSpec { name: "furigana", detail: "Expression with HTML ruby furigana" },
    MarkerSpec { name: "furigana-plain", detail: "Expression with bracket furigana" },
    MarkerSpec { name: "glossary", detail: "Dictionary definitions" },
    MarkerSpec { name: "glossary-brief", detail: "Definitions without dictionary headers" },
    MarkerSpec { name: "glossary-compact", detail: "Compact dictionary definitions" },
    MarkerSpec { name: "selection", detail: "Selected glossary text" },
    MarkerSpec { name: "part-of-speech", detail: "Part-of-speech tags" },
    MarkerSpec { name: "tags", detail: "Expression tags list" },
    MarkerSpec { name: "tags-brief", detail: "Expression tags without descriptions" },
    MarkerSpec { name: "subtitle", detail: "Active primary subtitle text" },
    MarkerSpec { name: "subtitle-2", detail: "Active secondary subtitle text" },
    MarkerSpec { name: "context", detail: "Selected primary subtitle context lines" },
    MarkerSpec { name: "context-2", detail: "Overlapping secondary subtitle context" },
    MarkerSpec { name: "cloze-body", detail: "Matched expression in subtitle" },
    MarkerSpec { name: "cloze-prefix", detail: "Subtitle text before {cloze-body}" },
    MarkerSpec { name: "cloze-suffix", detail: "Subtitle text after {cloze-body}" },
    MarkerSpec { name: "clipboard", detail: "Current clipboard text" },
    MarkerSpec { name: "title", detail: "Current media title or filename" },
    MarkerSpec { name: "pitch", detail: "Pitch accents for reading" },
    MarkerSpec { name: "pitch-graphs", detail: "Pitch accent graphs" },
    MarkerSpec { name: "pitch-positions", detail: "Pitch downstep positions" },
    MarkerSpec { name: "pitch-categories", detail: "Pitch categories (heiban, atamadaka, …)" },
    MarkerSpec { name: "frequencies", detail: "Bulleted frequency tags" },
    MarkerSpec { name: "frequency-harmonic-rank", detail: "Harmonic mean frequency rank" },
    MarkerSpec { name: "frequency-average-rank", detail: "Average frequency rank" },
    MarkerSpec { name: "frequency-harmonic-occurrence", detail: "Harmonic mean occurrences" },
    MarkerSpec { name: "frequency-average-occurrence", detail: "Average occurrences" },
    MarkerSpec { name: "audio", detail: "Word pronunciation audio" },
    MarkerSpec { name: "audio-media", detail: "Current subtitle audio clip" },
    MarkerSpec { name: "audio-context", detail: "Marked A-B or multi-line context audio" },
    MarkerSpec { name: "screenshot", detail: "Screenshot of current frame" },
    MarkerSpec { name: "screenshot-video", detail: "Clean screenshot without subtitles" },
    MarkerSpec { name: "video", detail: "MP4 video clip of current context" },
];

const KANJI_MARKERS: &[MarkerSpec] = &[
    MarkerSpec { name: "character", detail: "The kanji character" },
    MarkerSpec { name: "onyomi", detail: "Onyomi readings (hiragana)" },
    MarkerSpec { name: "kunyomi", detail: "Kunyomi readings (katakana)" },
    MarkerSpec { name: "stroke-count", detail: "Number of strokes" },
    MarkerSpec { name: "glossary", detail: "Kanji dictionary meanings" },
    MarkerSpec { name: "tags", detail: "Kanji tags list" },
    MarkerSpec { name: "tags-brief", detail: "Kanji tags without descriptions" },
    MarkerSpec { name: "subtitle", detail: "Active primary subtitle text" },
    MarkerSpec { name: "subtitle-2", detail: "Active secondary subtitle text" },
    MarkerSpec { name: "context", detail: "Selected primary subtitle context lines" },
    MarkerSpec { name: "context-2", detail: "Overlapping secondary subtitle context" },
    MarkerSpec { name: "cloze-body", detail: "Matched text in subtitle" },
    MarkerSpec { name: "cloze-prefix", detail: "Subtitle text before {cloze-body}" },
    MarkerSpec { name: "cloze-suffix", detail: "Subtitle text after {cloze-body}" },
    MarkerSpec { name: "clipboard", detail: "Current clipboard text" },
    MarkerSpec { name: "title", detail: "Current media title or filename" },
    MarkerSpec { name: "frequencies", detail: "Bulleted frequency tags" },
    MarkerSpec { name: "frequency-harmonic-rank", detail: "Harmonic mean frequency rank" },
    MarkerSpec { name: "frequency-average-rank", detail: "Average frequency rank" },
    MarkerSpec { name: "frequency-harmonic-occurrence", detail: "Harmonic mean occurrences" },
    MarkerSpec { name: "frequency-average-occurrence", detail: "Average occurrences" },
    MarkerSpec { name: "audio-media", detail: "Current subtitle audio clip" },
    MarkerSpec { name: "audio-context", detail: "Marked A-B or multi-line context audio" },
    MarkerSpec { name: "screenshot", detail: "Screenshot of current frame" },
    MarkerSpec { name: "screenshot-video", detail: "Clean screenshot without subtitles" },
    MarkerSpec { name: "video", detail: "MP4 video clip of current context" },
];

const BOOL_VALUES: &[&str] = &["true", "false"];
const SOURCE_VALUES: &[&str] = &["subtitle", "context"];

const ARG_NORM: ArgSpec = ArgSpec {
    name: "norm",
    detail: "Normalize audio loudness (true / false)",
    values: BOOL_VALUES,
};
const ARG_DB: ArgSpec = ArgSpec {
    name: "db",
    detail: "Normalization target dB (e.g. -20)",
    values: &[],
};
const ARG_SOURCE: ArgSpec = ArgSpec {
    name: "source",
    detail: "Clip timing source (subtitle / context)",
    values: SOURCE_VALUES,
};
const ARG_MIN_VALUE: ArgSpec = ArgSpec {
    name: "min-value",
    detail: "Export only lowest frequency (default: false)",
    values: BOOL_VALUES,
};
const ARG_VALUE_ONLY: ArgSpec = ArgSpec {
    name: "value-only",
    detail: "Omit dictionary names (default: false)",
    values: BOOL_VALUES,
};
const ARG_DICT: ArgSpec = ArgSpec {
    name: "dict",
    detail: "Filter by dictionary ID (see Dictionaries tab)",
    values: &[],
};
const ARG_MAX_WIDTH: ArgSpec = ArgSpec {
    name: "max-width",
    detail: "Max screenshot width px (-1 unbounded)",
    values: &[],
};
const ARG_MAX_HEIGHT: ArgSpec = ArgSpec {
    name: "max-height",
    detail: "Max screenshot height px (-1 unbounded)",
    values: &[],
};
const ARG_KEEP_RATIO: ArgSpec = ArgSpec {
    name: "keep-ratio",
    detail: "Preserve aspect ratio (default: true)",
    values: BOOL_VALUES,
};
const ARG_AUDIO: ArgSpec = ArgSpec {
    name: "audio",
    detail: "Include audio track (default: true)",
    values: BOOL_VALUES,
};
const ARG_SUBTITLES: ArgSpec = ArgSpec {
    name: "subtitles",
    detail: "Burn subtitles into clip (default: true)",
    values: BOOL_VALUES,
};

fn marker_arguments(marker_name: &str, is_kanji: bool) -> &'static [ArgSpec] {
    match marker_name {
        "audio-context" => &[ARG_NORM, ARG_DB],
        "audio-media" => &[ARG_SOURCE, ARG_NORM, ARG_DB],
        "frequencies" => &[ARG_MIN_VALUE, ARG_VALUE_ONLY],
        "glossary" if !is_kanji => &[ARG_DICT],
        "glossary-brief" | "glossary-compact" => &[ARG_DICT],
        "screenshot" | "screenshot-video" => &[ARG_MAX_WIDTH, ARG_MAX_HEIGHT, ARG_KEEP_RATIO],
        "video" => &[ARG_SOURCE, ARG_AUDIO, ARG_SUBTITLES, ARG_NORM, ARG_DB],
        _ => &[],
    }
}

#[derive(Clone, Debug)]
pub(crate) struct AutocompleteResult {
    pub(crate) items: Vec<AnkiAutocompleteItem>,
    pub(crate) replace_start_byte: usize,
    pub(crate) replace_end_byte: usize,
    pub(crate) is_argument: bool,
}

#[derive(Clone, Debug)]
struct ActiveAutocompleteContext {
    field_key: String,
    query_text: String,
    replace_start_byte: usize,
    replace_end_byte: usize,
    is_argument: bool,
}

fn byte_index_for_char_pos(text: &str, char_pos: usize) -> usize {
    if char_pos == 0 {
        return 0;
    }
    text.char_indices()
        .nth(char_pos)
        .map_or(text.len(), |(idx, _)| idx)
}

/// Computes Anki marker or argument autocomplete suggestions at `cursor_chars`
/// inside `text` for a Term or Kanji note field (`field_key`).
pub(crate) fn compute_anki_autocomplete(
    field_key: &str,
    text: &str,
    cursor_chars: usize,
) -> Option<AutocompleteResult> {
    let cursor_byte = byte_index_for_char_pos(text, cursor_chars);
    let before = &text[..cursor_byte];
    let open_idx = before.rfind('{')?;
    let inside = &before[open_idx + 1..];
    // Already closed before the cursor.
    if inside.contains('}') {
        return None;
    }

    let is_kanji = field_key.contains("/kanji/");
    let (seg_offset_in_inside, segment) = match inside.rfind('|') {
        Some(pipe_idx) => (pipe_idx + 1, &inside[pipe_idx + 1..]),
        None => (0, inside),
    };
    let seg_start_byte = open_idx + 1 + seg_offset_in_inside;

    if let Some(colon_idx) = segment.find(':') {
        // Argument name / value autocomplete after `:` or `,`
        let marker_name = segment[..colon_idx].trim().to_ascii_lowercase();
        let valid_args = marker_arguments(&marker_name, is_kanji);
        if valid_args.is_empty() {
            return None;
        }
        let args_part = &segment[colon_idx + 1..];
        let args_start_byte = seg_start_byte + colon_idx + 1;

        // Collect already-specified argument names before the last comma.
        let (last_arg_offset, current_arg_raw, prior_args_part) = match args_part.rfind(',') {
            Some(comma_idx) => (
                comma_idx + 1,
                &args_part[comma_idx + 1..],
                &args_part[..comma_idx],
            ),
            None => (0, args_part, ""),
        };
        let current_arg_start = args_start_byte + last_arg_offset;

        if let Some(eq_idx) = current_arg_raw.find('=') {
            // Value autocomplete after `arg = `
            let arg_name = current_arg_raw[..eq_idx].trim().to_ascii_lowercase();
            let spec = valid_args.iter().find(|a| a.name == arg_name)?;
            if spec.values.is_empty() {
                return None;
            }
            let val_raw = &current_arg_raw[eq_idx + 1..];
            let val_trimmed = val_raw.trim_start();
            let val_prefix = val_trimmed.to_ascii_lowercase();
            if val_prefix.chars().any(char::is_whitespace) {
                return None;
            }
            let replace_start_byte =
                current_arg_start + eq_idx + 1 + (val_raw.len() - val_trimmed.len());
            let items: Vec<AnkiAutocompleteItem> = spec
                .values
                .iter()
                .filter(|v| v.starts_with(&val_prefix))
                .map(|&v| AnkiAutocompleteItem {
                    insert_text: v.into(),
                    label: v.into(),
                    detail: format!("Value for {}", spec.name).into(),
                    kind: "argument".into(),
                })
                .collect();
            if items.is_empty() {
                return None;
            }
            return Some(AutocompleteResult {
                items,
                replace_start_byte,
                replace_end_byte: cursor_byte,
                is_argument: true,
            });
        }

        let used_args: Vec<String> = prior_args_part
            .split(',')
            .filter_map(|part| part.split('=').next())
            .map(|name| name.trim().to_ascii_lowercase())
            .filter(|name| !name.is_empty())
            .collect();

        let trimmed = current_arg_raw.trim_start();
        let prefix = trimmed.to_ascii_lowercase();
        if prefix.chars().any(char::is_whitespace) {
            return None;
        }
        let replace_start_byte = current_arg_start + (current_arg_raw.len() - trimmed.len());
        let items: Vec<AnkiAutocompleteItem> = valid_args
            .iter()
            .filter(|arg| !used_args.iter().any(|u| u == arg.name))
            .filter(|arg| arg.name.starts_with(&prefix))
            .map(|arg| AnkiAutocompleteItem {
                insert_text: format!("{} = ", arg.name).into(),
                label: arg.name.into(),
                detail: arg.detail.into(),
                kind: "argument".into(),
            })
            .collect();
        if items.is_empty() {
            return None;
        }
        return Some(AutocompleteResult {
            items,
            replace_start_byte,
            replace_end_byte: cursor_byte,
            is_argument: true,
        });
    }

    // Marker autocomplete after `{` or `|`
    let trimmed = segment.trim_start();
    let prefix = trimmed.to_ascii_lowercase();
    if prefix.chars().any(char::is_whitespace) {
        return None;
    }
    let replace_start_byte = seg_start_byte + (segment.len() - trimmed.len());
    let catalog = if is_kanji {
        KANJI_MARKERS
    } else {
        TERM_MARKERS
    };

    let items: Vec<AnkiAutocompleteItem> = catalog
        .iter()
        .filter(|spec| spec.name.starts_with(&prefix))
        .map(|spec| AnkiAutocompleteItem {
            insert_text: spec.name.into(),
            label: format!("{{{}}}", spec.name).into(),
            detail: spec.detail.into(),
            kind: "marker".into(),
        })
        .collect();
    if items.is_empty() {
        return None;
    }
    Some(AutocompleteResult {
        items,
        replace_start_byte,
        replace_end_byte: cursor_byte,
        is_argument: false,
    })
}

/// Applies an accepted autocomplete suggestion into `query_text` between
/// `[replace_start_byte..replace_end_byte]`.
pub(crate) fn apply_anki_autocomplete_acceptance(
    query_text: &str,
    replace_start_byte: usize,
    replace_end_byte: usize,
    insert_text: &str,
    is_argument: bool,
) -> String {
    let start = replace_start_byte.min(query_text.len());
    let end = replace_end_byte.clamp(start, query_text.len());
    let prefix = &query_text[..start];
    let suffix = &query_text[end..];

    let mut out = String::with_capacity(query_text.len() + insert_text.len() + 2);
    out.push_str(prefix);
    out.push_str(insert_text);
    if !is_argument {
        let trimmed_suffix = suffix.trim_start();
        let already_delimited = trimmed_suffix.starts_with('}')
            || trimmed_suffix.starts_with('|')
            || trimmed_suffix.starts_with(':');
        if !already_delimited {
            out.push('}');
        }
    }
    out.push_str(suffix);
    out
}

fn current_anki_field_value(config: &ConfigController, field_key: &str) -> String {
    for group in config.category_settings_groups(SettingsCategory::Anki.as_i32()) {
        for fi in 0..group.fields.row_count() {
            if let Some(field) = group.fields.row_data(fi) {
                if field.key.as_str() == field_key {
                    return field.value.to_string();
                }
            }
        }
    }
    String::new()
}

// ── Excluded Glossaries (`ex-glos`) Checklist Sync ──────────────────────────

fn installed_dictionary_names(
    window: &SettingsWindow,
    manager: Option<&DictionaryManager>,
    config: &ConfigController,
) -> Vec<String> {
    if let Some(mgr) = manager {
        if let Ok(dicts) = mgr.list_dictionaries() {
            return ordered_dictionaries(dicts, config)
                .into_iter()
                .map(|d| d.title)
                .collect();
        }
    }
    let model = window.get_installed_dictionaries();
    (0..model.row_count())
        .filter_map(|idx| model.row_data(idx))
        .map(|row| row.title.to_string())
        .collect()
}

pub(crate) fn refresh_anki_excluded_dicts(
    window: &SettingsWindow,
    manager: Option<&DictionaryManager>,
    config: &ConfigController,
) {
    let installed = installed_dictionary_names(window, manager, config);
    let checklist = config.anki_excluded_glossaries_checklist(&installed);
    let rows: Vec<AnkiExcludedDictRow> = checklist
        .into_iter()
        .map(|item| {
            let is_installed = installed
                .iter()
                .any(|name| name.trim() == item.dictionary_name);
            AnkiExcludedDictRow {
                name: item.dictionary_name.into(),
                excluded: item.excluded,
                installed: is_installed,
            }
        })
        .collect();
    window.set_anki_excluded_dicts(Rc::new(VecModel::from(rows)).into());
}

pub(crate) fn refresh_anki_excluded_dicts_from_window(
    window: &SettingsWindow,
    config: &ConfigController,
) {
    refresh_anki_excluded_dicts(window, None, config);
}

// ── Silent Auto-Fetch of Decks & Models on Open ─────────────────────────────

pub(crate) fn reset_auto_fetch_attempt(ctx: &SettingsContext) {
    ctx.anki_auto_fetch_attempted.set(false);
}

/// Automatically kicks off a silent, non-blocking decks + models fetch when
/// opening the Anki Settings page if Anki integration is enabled and lists
/// have not yet been populated.
pub(crate) fn maybe_auto_fetch_decks_and_models(window: &SettingsWindow, ctx: &SettingsContext) {
    if !ctx.config.anki_enabled() {
        return;
    }
    let decks_empty = window.get_available_anki_decks().row_count() == 0;
    let models_empty = window.get_available_anki_models().row_count() == 0;
    if !decks_empty && !models_empty {
        return;
    }
    if window.get_testing_anki_connection() {
        return;
    }
    if ctx.anki_auto_fetch_attempted.get() {
        return;
    }
    if ctx
        .anki_auto_fetch_in_flight
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    ctx.anki_auto_fetch_attempted.set(true);

    let endpoint = ctx.config.anki_endpoint();
    let in_flight = Arc::clone(&ctx.anki_auto_fetch_in_flight);
    let response_window = window.as_weak();
    let _ = std::thread::Builder::new()
        .name("subomi-anki-autofetch".to_owned())
        .spawn(move || {
            let controller = AnkiController::new(endpoint);
            let fetched = controller.client().deck_names().and_then(|decks| {
                let models = controller.client().model_names()?;
                Ok((decks, models))
            });
            in_flight.store(false, Ordering::Release);
            if let Ok((decks, models)) = fetched {
                let _ = response_window.upgrade_in_event_loop(move |win| {
                    win.set_available_anki_decks(
                        Rc::new(VecModel::from(
                            decks.into_iter().map(Into::into).collect::<Vec<_>>(),
                        ))
                        .into(),
                    );
                    win.set_available_anki_models(
                        Rc::new(VecModel::from(
                            models.into_iter().map(Into::into).collect::<Vec<_>>(),
                        ))
                        .into(),
                    );
                });
            }
        });
}

fn dismiss_autocomplete(
    window: &SettingsWindow,
    active_ctx: &RefCell<Option<ActiveAutocompleteContext>>,
) {
    *active_ctx.borrow_mut() = None;
    window.set_anki_autocomplete_open(false);
    window.set_anki_autocomplete_field_key("".into());
    window.set_anki_autocomplete_selected(0);
    window.set_anki_autocomplete_items(Rc::new(VecModel::<AnkiAutocompleteItem>::default()).into());
}

pub(crate) fn wire(window: &SettingsWindow, ctx: &Rc<SettingsContext>) {
    // A native Help window on Wayland becomes visible when it is created.
    // Construct it lazily on first request and keep its light/dark theme synced.
    let help_settings_window = window.as_weak();
    let help_ctx = ctx.clone();
    window.on_anki_help_requested(move || {
        let is_light = help_settings_window
            .upgrade()
            .map_or(false, |w| w.get_is_light_theme());
        if help_ctx.help_window.borrow().is_none() {
            match AnkiHelpWindow::new() {
                Ok(win) => {
                    anki_help::populate(&win);
                    win.set_is_light_theme(is_light);
                    let theme_settings_win = help_settings_window.clone();
                    let theme_ctx = help_ctx.clone();
                    win.window().on_winit_window_event(move |_winit_win, event| {
                        if let WindowEvent::ThemeChanged(winit_theme) = event {
                            let scheme = match winit_theme {
                                WinitTheme::Light => SystemColorScheme::Light,
                                WinitTheme::Dark => SystemColorScheme::Dark,
                            };
                            if let Some(settings_win) = theme_settings_win.upgrade() {
                                super::sync_theme(&settings_win, &theme_ctx, Some(scheme));
                            }
                        }
                        EventResult::Propagate
                    });
                    *help_ctx.help_window.borrow_mut() = Some(win);
                }
                Err(error) => {
                    eprintln!("Could not create Anki help: {error}");
                    return;
                }
            }
        }
        if let Some(win) = help_ctx.help_window.borrow().as_ref() {
            win.set_is_light_theme(is_light);
            if let Err(error) = win.show() {
                eprintln!("Could not open Anki help: {error}");
            }
        }
    });

    // Excluded glossary dictionary checklist (`ex-glos`)
    let ex_window = window.as_weak();
    let ex_ctx = ctx.clone();
    window.on_anki_excluded_dict_toggled(move |dict_name, excluded| {
        let Some(window) = ex_window.upgrade() else {
            return;
        };
        ex_ctx.snapshot_category_if_clean(SettingsCategory::Anki);
        match ex_ctx.config.set_anki_glossary_excluded(&dict_name, excluded) {
            Ok(()) => {
                ex_ctx.mark_dirty(SettingsCategory::Anki);
                refresh_anki_excluded_dicts(
                    &window,
                    ex_ctx.dictionary_manager.as_deref(),
                    &ex_ctx.config,
                );
                sync_after_edit(&window, &ex_ctx.config);
                window.set_settings_status("Unsaved changes — click Apply".into());
            }
            Err(error) => window.set_settings_status(error.into()),
        }
    });

    // Marker & argument autocomplete popup for `AnkiFieldEditorControl`
    let ac_context: Rc<RefCell<Option<ActiveAutocompleteContext>>> = Rc::new(RefCell::new(None));

    let ac_query_window = window.as_weak();
    let ac_query_ctx = ac_context.clone();
    window.on_anki_field_autocomplete_query(move |field_key, text, cursor_pos| {
        let Some(window) = ac_query_window.upgrade() else {
            return;
        };
        let cursor_chars = cursor_pos.max(0) as usize;
        match compute_anki_autocomplete(&field_key, &text, cursor_chars) {
            Some(result) => {
                *ac_query_ctx.borrow_mut() = Some(ActiveAutocompleteContext {
                    field_key: field_key.to_string(),
                    query_text: text.to_string(),
                    replace_start_byte: result.replace_start_byte,
                    replace_end_byte: result.replace_end_byte,
                    is_argument: result.is_argument,
                });
                window.set_anki_autocomplete_field_key(field_key);
                window.set_anki_autocomplete_selected(0);
                window.set_anki_autocomplete_items(Rc::new(VecModel::from(result.items)).into());
                window.set_anki_autocomplete_open(true);
            }
            None => {
                dismiss_autocomplete(&window, &ac_query_ctx);
            }
        }
    });

    let ac_accept_window = window.as_weak();
    let ac_accept_settings_ctx = ctx.clone();
    let ac_accept_ctx = ac_context.clone();
    window.on_anki_field_autocomplete_accepted(move |field_key, insert_text| {
        let Some(window) = ac_accept_window.upgrade() else {
            return;
        };
        let saved = ac_accept_ctx
            .borrow_mut()
            .take()
            .filter(|c| c.field_key == field_key.as_str());
        let updated = if let Some(saved) = saved {
            apply_anki_autocomplete_acceptance(
                &saved.query_text,
                saved.replace_start_byte,
                saved.replace_end_byte,
                &insert_text,
                saved.is_argument,
            )
        } else {
            let current = current_anki_field_value(&ac_accept_settings_ctx.config, &field_key);
            if let Some(res) =
                compute_anki_autocomplete(&field_key, &current, current.chars().count())
            {
                apply_anki_autocomplete_acceptance(
                    &current,
                    res.replace_start_byte,
                    res.replace_end_byte,
                    &insert_text,
                    res.is_argument,
                )
            } else {
                format!("{current}{{{insert_text}}}")
            }
        };

        dismiss_autocomplete(&window, &ac_accept_ctx);
        ac_accept_settings_ctx.snapshot_category_if_clean(SettingsCategory::Anki);
        match ac_accept_settings_ctx
            .config
            .edit_setting(&field_key, &updated)
        {
            Ok(()) => {
                ac_accept_settings_ctx.mark_dirty(SettingsCategory::Anki);
                sync_after_edit(&window, &ac_accept_settings_ctx.config);
                window.set_settings_status("Unsaved changes — click Apply".into());
            }
            Err(error) => window.set_settings_status(error.into()),
        }
    });

    let ac_dismiss_window = window.as_weak();
    let ac_dismiss_ctx = ac_context;
    window.on_anki_field_autocomplete_dismissed(move || {
        if let Some(window) = ac_dismiss_window.upgrade() {
            dismiss_autocomplete(&window, &ac_dismiss_ctx);
        }
    });

    let preset_window = window.as_weak();
    let preset_ctx = ctx.clone();
    window.on_apply_anki_preset(move |preset_name| {
        let Some(window) = preset_window.upgrade() else {
            return;
        };
        preset_ctx.snapshot_category_if_clean(SettingsCategory::Anki);
        match preset_ctx.config.apply_anki_note_preset(&preset_name) {
            Ok(count) => {
                preset_ctx.mark_dirty(SettingsCategory::Anki);
                refresh_category_settings(
                    &window,
                    &preset_ctx.config,
                    SettingsCategory::Anki.as_i32(),
                );
                window.set_settings_status(
                    format!("Applied {preset_name} preset ({count} fields) — click Apply to save")
                        .into(),
                );
            }
            Err(error) => window.set_settings_status(error.into()),
        }
    });

    // Guarded Test Connection (prevents overlapping concurrent network calls)
    let test_window = window.as_weak();
    let test_ctx = ctx.clone();
    window.on_test_anki_connection(move || {
        let Some(window) = test_window.upgrade() else {
            return;
        };
        if window.get_testing_anki_connection() {
            return;
        }
        window.set_testing_anki_connection(true);
        window.set_settings_status("Testing AnkiConnect connection…".into());
        let endpoint = test_ctx.config.anki_endpoint();
        let response_window = test_window.clone();
        std::thread::spawn(move || {
            let controller = AnkiController::new(endpoint);
            let result = controller.test_connection().and_then(|version| {
                let decks = controller
                    .client()
                    .deck_names()
                    .map_err(|error| error.to_string())?;
                let models = controller
                    .client()
                    .model_names()
                    .map_err(|error| error.to_string())?;
                Ok((version, decks, models))
            });
            let _ = response_window.upgrade_in_event_loop(move |window| {
                window.set_testing_anki_connection(false);
                let (ok, text) = match result {
                    Ok((version, decks, models)) => {
                        window.set_available_anki_decks(
                            Rc::new(VecModel::from(
                                decks.into_iter().map(Into::into).collect::<Vec<_>>(),
                            ))
                            .into(),
                        );
                        window.set_available_anki_models(
                            Rc::new(VecModel::from(
                                models.into_iter().map(Into::into).collect::<Vec<_>>(),
                            ))
                            .into(),
                        );
                        (
                            true,
                            format!(
                                "AnkiConnect is running (API version {version}). Decks and models are ready to select."
                            ),
                        )
                    }
                    Err(error) => (
                        false,
                        format!(
                            "Could not reach AnkiConnect: {error}\nMake sure Anki is open and the AnkiConnect add-on is installed."
                        ),
                    ),
                };
                window.set_settings_status(
                    if ok {
                        "Anki connection OK"
                    } else {
                        "Anki connection failed"
                    }
                    .into(),
                );
                window.set_connection_popup_ok(ok);
                window.set_connection_popup_text(text.into());
                window.set_connection_popup_open(true);
            });
        });
    });

    // Event-driven Anki note model field loading (no 100ms polling Timer)
    let pending_fields: Arc<Mutex<VecDeque<PendingAnkiFields>>> =
        Arc::new(Mutex::new(VecDeque::new()));

    let loaded_window = window.as_weak();
    let loaded_ctx = ctx.clone();
    let loaded_queue = pending_fields.clone();
    window.on_anki_fields_loaded(move || {
        let Some(window) = loaded_window.upgrade() else {
            return;
        };
        let items: Vec<PendingAnkiFields> = loaded_queue
            .lock()
            .map(|mut q| q.drain(..).collect())
            .unwrap_or_default();
        for item in items {
            if loaded_ctx.config.current_anki_profile() != item.profile_name
                || loaded_ctx.config.anki_note_model(&item.section).as_deref()
                    != Some(item.model.as_str())
            {
                continue;
            }
            match item.result {
                Ok(fields) if !fields.is_empty() => {
                    loaded_ctx.snapshot_category_if_clean(SettingsCategory::Anki);
                    match loaded_ctx.config.set_anki_model_fields(&item.section, &fields) {
                        Ok(()) => {
                            loaded_ctx.mark_dirty(SettingsCategory::Anki);
                            refresh_category_settings(
                                &window,
                                &loaded_ctx.config,
                                SettingsCategory::Anki.as_i32(),
                            );
                            window.set_settings_status(
                                format!(
                                    "Loaded {} model fields — click Apply to save",
                                    fields.len()
                                )
                                .into(),
                            );
                        }
                        Err(error) => window.set_settings_status(
                            format!("Could not update Anki fields: {error}").into(),
                        ),
                    }
                }
                Ok(_) => {
                    window.set_settings_status("The selected Anki model has no fields".into())
                }
                Err(error) => window.set_settings_status(
                    format!("Could not load Anki fields: {error}").into(),
                ),
            }
        }
    });

    let load_window = window.as_weak();
    let load_ctx = ctx.clone();
    window.on_load_anki_fields(move |section| {
        let Some(window) = load_window.upgrade() else {
            return;
        };
        let section = section.to_string();
        let Some(model) = load_ctx
            .config
            .anki_note_model(&section)
            .filter(|model| !model.trim().is_empty())
        else {
            window.set_settings_status("Choose an Anki note model first".into());
            return;
        };
        window.set_settings_status(format!("Loading {model} fields from Anki…").into());
        let endpoint = load_ctx.config.anki_endpoint();
        let profile_name = load_ctx.config.current_anki_profile();
        let queue = pending_fields.clone();
        let notify_window = load_window.clone();
        std::thread::spawn(move || {
            let result = AnkiController::new(endpoint)
                .client()
                .field_names(&model)
                .map_err(|error| error.to_string());
            if let Ok(mut guard) = queue.lock() {
                guard.push_back(PendingAnkiFields {
                    section,
                    profile_name,
                    model,
                    result,
                });
            }
            let _ = notify_window.upgrade_in_event_loop(|win| win.invoke_anki_fields_loaded());
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anki_autocomplete_suggests_term_and_kanji_markers_and_arguments() {
        let term_key = "anki/profile/term/field/Glossary";
        let kanji_key = "anki/profile/kanji/field/Meaning";

        // After `{` with prefix `glo` on Term field
        let res = compute_anki_autocomplete(term_key, "{glo", 4).expect("term glossary markers");
        let names: Vec<String> = res.items.iter().map(|i| i.insert_text.to_string()).collect();
        assert_eq!(names, vec!["glossary", "glossary-brief", "glossary-compact"]);
        assert!(res.items.iter().all(|i| i.kind == "marker"));
        assert!(!res.is_argument);
        assert_eq!(
            apply_anki_autocomplete_acceptance(
                "{glo",
                res.replace_start_byte,
                res.replace_end_byte,
                "glossary-compact",
                res.is_argument,
            ),
            "{glossary-compact}"
        );

        // Kanji field exposes kanji markers (`character`, `onyomi`, `kunyomi`) and omits `expression`
        let kanji_all = compute_anki_autocomplete(kanji_key, "{", 1).expect("kanji markers");
        assert!(kanji_all.items.iter().any(|i| i.insert_text == "character"));
        assert!(kanji_all.items.iter().any(|i| i.insert_text == "onyomi"));
        assert!(!kanji_all.items.iter().any(|i| i.insert_text == "expression"));

        // Fallback chain after `|` preserves prefix and closes brace cleanly
        let chain = compute_anki_autocomplete(term_key, "{selection | cl", 15).expect("fallback");
        assert!(chain.items.iter().any(|i| i.insert_text == "clipboard"));
        assert_eq!(
            apply_anki_autocomplete_acceptance(
                "{selection | cl",
                chain.replace_start_byte,
                chain.replace_end_byte,
                "clipboard",
                chain.is_argument,
            ),
            "{selection | clipboard}"
        );

        // Arguments after `:` and `,`
        let args = compute_anki_autocomplete(term_key, "{screenshot : max-", 18).expect("args");
        let arg_labels: Vec<String> = args.items.iter().map(|i| i.label.to_string()).collect();
        assert_eq!(arg_labels, vec!["max-width", "max-height"]);
        assert!(args.items.iter().all(|i| i.kind == "argument"));
        assert!(args.is_argument);
        assert_eq!(
            apply_anki_autocomplete_acceptance(
                "{screenshot : max-",
                args.replace_start_byte,
                args.replace_end_byte,
                "max-width = ",
                args.is_argument,
            ),
            "{screenshot : max-width = "
        );

        // Already-used argument omitted after `,`
        let next_args =
            compute_anki_autocomplete(term_key, "{screenshot : max-width = 1280, ", 32)
                .expect("remaining args");
        assert!(!next_args.items.iter().any(|i| i.label == "max-width"));
        assert!(next_args.items.iter().any(|i| i.label == "max-height"));
        assert!(next_args.items.iter().any(|i| i.label == "keep-ratio"));

        // Term `{glossary :` offers `dict`, whereas Kanji `{glossary :` has no arguments
        let term_dict = compute_anki_autocomplete(term_key, "{glossary : ", 12).expect("dict arg");
        assert_eq!(term_dict.items.len(), 1);
        assert_eq!(term_dict.items[0].label, "dict");
        assert!(compute_anki_autocomplete(kanji_key, "{glossary : ", 12).is_none());

        // Closed marker returns None
        assert!(compute_anki_autocomplete(term_key, "{expression}", 12).is_none());

        // Multi-byte Japanese prefix before `{` computes exact UTF-8 byte offsets
        let jp = "日本語{exp";
        let jp_res = compute_anki_autocomplete(term_key, jp, jp.chars().count()).expect("jp");
        assert_eq!(
            apply_anki_autocomplete_acceptance(
                jp,
                jp_res.replace_start_byte,
                jp_res.replace_end_byte,
                "expression",
                jp_res.is_argument,
            ),
            "日本語{expression}"
        );
    }
}

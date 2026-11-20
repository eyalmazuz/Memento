//! Keyboard shortcut translation (Slint PUA keys, bare modifiers, macOS `Cmd`,
//! complete mpv key forwarding), popup term-group navigation, manual A/B audio
//! context marking, and main-window key event dispatch.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use slint::{ComponentHandle, Model as _};

use crate::controller::config::{
    ConfigController, KEYBIND_CLEAR_AUDIO_CONTEXT, KEYBIND_MARK_AUDIO_CONTEXT,
    SETTINGS_SHORTCUT_DEFAULT, SETTINGS_SHORTCUT_MACOS,
};
use crate::ui::{DictResult, MementoApp, PopupBridge, SubtitleOverlayBridge};
use memento_config::Value;
use memento_player::MpvInstance;
use memento_setting::keys as raw;

/// Manual A/B audio context marking state (`H5.2 / #136`).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) enum AudioContextMarkState {
    #[default]
    Empty,
    MarkedA(f64),
    MarkedRange {
        start: f64,
        end: f64,
    },
}

/// Format playback timestamps deterministically as `MM:SS.cc` or `HH:MM:SS.cc`
/// so `app-anki-bridge` can parse exact `[start, end]` ranges from `audio-context-hud-text`.
pub(crate) fn format_precise_ts(seconds: f64) -> String {
    let clamped = if seconds.is_finite() {
        seconds.max(0.0)
    } else {
        0.0
    };
    let total_centis = (clamped * 100.0).round() as u64;
    let centis = total_centis % 100;
    let total_secs = total_centis / 100;
    let secs = total_secs % 60;
    let total_mins = total_secs / 60;
    let mins = total_mins % 60;
    let hours = total_mins / 60;
    if hours > 0 {
        format!("{hours:02}:{mins:02}:{secs:02}.{centis:02}")
    } else {
        format!("{mins:02}:{secs:02}.{centis:02}")
    }
}

impl AudioContextMarkState {
    pub(crate) fn mark_at(&mut self, pos_sec: f64) -> String {
        let pos = if pos_sec.is_finite() {
            pos_sec.max(0.0)
        } else {
            0.0
        };
        match *self {
            Self::Empty | Self::MarkedRange { .. } => {
                *self = Self::MarkedA(pos);
                format!("Audio Context: {} → ...", format_precise_ts(pos))
            }
            Self::MarkedA(a) => {
                let (start, end) = if pos < a { (pos, a) } else { (a, pos) };
                *self = Self::MarkedRange { start, end };
                format!(
                    "Audio Context: {} → {}",
                    format_precise_ts(start),
                    format_precise_ts(end)
                )
            }
        }
    }

    pub(crate) fn clear(&mut self) {
        *self = Self::Empty;
    }
}

pub(crate) fn is_bare_modifier_key(key: &str) -> bool {
    matches!(
        key,
        "" | "\u{0010}"
            | "\u{0011}"
            | "\u{0012}"
            | "\u{0013}"
            | "\u{0014}"
            | "\u{0015}"
            | "\u{0016}"
            | "\u{0017}"
            | "\u{0018}"
            | "Shift"
            | "ShiftR"
            | "Control"
            | "Ctrl"
            | "Alt"
            | "Meta"
            | "Super"
    )
}

pub(crate) fn bare_modifier_flags(key: &str) -> (bool, bool, bool, bool) {
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

/// Translate Slint's special/PUA key codepoints to canonical shortcut names,
/// matching `settings_ui::format_key_chord`.
pub(crate) fn translate_slint_key_name(key: &str) -> Option<&'static str> {
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
        "\u{F710}" => Some("F13"),
        "\u{F711}" => Some("F14"),
        "\u{F712}" => Some("F15"),
        "\u{F713}" => Some("F16"),
        "\u{F714}" => Some("F17"),
        "\u{F715}" => Some("F18"),
        "\u{F716}" => Some("F19"),
        "\u{F717}" => Some("F20"),
        "\u{F718}" => Some("F21"),
        "\u{F719}" => Some("F22"),
        "\u{F71A}" => Some("F23"),
        "\u{F71B}" => Some("F24"),
        "\u{F71C}" => Some("F25"),
        "\u{F71D}" => Some("F26"),
        "\u{F71E}" => Some("F27"),
        "\u{F71F}" => Some("F28"),
        "\u{F720}" => Some("F29"),
        "\u{F721}" => Some("F30"),
        "\u{F727}" => Some("Insert"),
        "\u{007f}" | "\u{F728}" => Some("Delete"),
        "\u{F729}" => Some("Home"),
        "\u{F72B}" => Some("End"),
        "\u{F72C}" => Some("PageUp"),
        "\u{F72D}" => Some("PageDown"),
        "\u{F730}" | "Pause" => Some("Pause"),
        "\u{001b}" | "Escape" => Some("Escape"),
        "\u{0008}" => Some("Backspace"),
        "\u{0009}" | "\u{0019}" => Some("Tab"),
        "\u{000a}" | "\r" => Some("Return"),
        " " => Some("Space"),
        _ => None,
    }
}

pub(crate) fn translate_mpv_key_name(key: &str) -> Option<&'static str> {
    match key {
        "\u{F700}" => Some("UP"),
        "\u{F701}" => Some("DOWN"),
        "\u{F702}" => Some("LEFT"),
        "\u{F703}" => Some("RIGHT"),
        "\u{0008}" => Some("BS"),
        "\u{000a}" | "\r" => Some("ENTER"),
        "\u{0009}" | "\u{0019}" => Some("TAB"),
        "\u{001b}" | "Escape" => Some("ESC"),
        " " | "Space" => Some("SPACE"),
        "\u{F727}" => Some("INS"),
        "\u{007f}" | "\u{F728}" => Some("DEL"),
        "\u{F729}" => Some("HOME"),
        "\u{F72B}" => Some("END"),
        "\u{F72C}" => Some("PGUP"),
        "\u{F72D}" => Some("PGDWN"),
        "\u{F730}" | "Pause" | "PAUSE" => Some("PAUSE"),
        "\u{F704}" | "F1" => Some("F1"),
        "\u{F705}" | "F2" => Some("F2"),
        "\u{F706}" | "F3" => Some("F3"),
        "\u{F707}" | "F4" => Some("F4"),
        "\u{F708}" | "F5" => Some("F5"),
        "\u{F709}" | "F6" => Some("F6"),
        "\u{F70A}" | "F7" => Some("F7"),
        "\u{F70B}" | "F8" => Some("F8"),
        "\u{F70C}" | "F9" => Some("F9"),
        "\u{F70D}" | "F10" => Some("F10"),
        "\u{F70E}" | "F11" => Some("F11"),
        "\u{F70F}" | "F12" => Some("F12"),
        "\u{F710}" | "F13" => Some("F13"),
        "\u{F711}" | "F14" => Some("F14"),
        "\u{F712}" | "F15" => Some("F15"),
        "\u{F713}" | "F16" => Some("F16"),
        "\u{F714}" | "F17" => Some("F17"),
        "\u{F715}" | "F18" => Some("F18"),
        "\u{F716}" | "F19" => Some("F19"),
        "\u{F717}" | "F20" => Some("F20"),
        "\u{F718}" | "F21" => Some("F21"),
        "\u{F719}" | "F22" => Some("F22"),
        "\u{F71A}" | "F23" => Some("F23"),
        "\u{F71B}" | "F24" => Some("F24"),
        "\u{F71C}" | "F25" => Some("F25"),
        "\u{F71D}" | "F26" => Some("F26"),
        "\u{F71E}" | "F27" => Some("F27"),
        "\u{F71F}" | "F28" => Some("F28"),
        "\u{F720}" | "F29" => Some("F29"),
        "\u{F721}" | "F30" => Some("F30"),
        "KP0" | "Numpad0" => Some("KP0"),
        "KP1" | "Numpad1" => Some("KP1"),
        "KP2" | "Numpad2" => Some("KP2"),
        "KP3" | "Numpad3" => Some("KP3"),
        "KP4" | "Numpad4" => Some("KP4"),
        "KP5" | "Numpad5" => Some("KP5"),
        "KP6" | "Numpad6" => Some("KP6"),
        "KP7" | "Numpad7" => Some("KP7"),
        "KP8" | "Numpad8" => Some("KP8"),
        "KP9" | "Numpad9" => Some("KP9"),
        "KP_ENTER" | "NumpadEnter" => Some("KP_ENTER"),
        "KP_DEC" | "KP_DECIMAL" | "NumpadDecimal" => Some("KP_DEC"),
        "KP_DEL" => Some("KP_DEL"),
        "KP_INS" => Some("KP_INS"),
        "KP_HOME" => Some("KP_HOME"),
        "KP_END" => Some("KP_END"),
        "KP_PGUP" => Some("KP_PGUP"),
        "KP_PGDWN" => Some("KP_PGDWN"),
        "KP_UP" => Some("KP_UP"),
        "KP_DOWN" => Some("KP_DOWN"),
        "KP_LEFT" => Some("KP_LEFT"),
        "KP_RIGHT" => Some("KP_RIGHT"),
        "KP_BEGIN" => Some("KP_BEGIN"),
        "KP_ADD" | "NumpadAdd" => Some("KP_ADD"),
        "KP_SUBTRACT" | "NumpadSubtract" => Some("KP_SUBTRACT"),
        "KP_MULTIPLY" | "NumpadMultiply" => Some("KP_MULTIPLY"),
        "KP_DIVIDE" | "NumpadDivide" => Some("KP_DIVIDE"),
        _ => None,
    }
}

/// Format a Slint key event into an mpv `keypress` argument using mpv's canonical
/// `Shift+Ctrl+Alt+Meta+` modifier order, supporting `F1..F30`, `PAUSE`, `KP_*`,
/// modified `SPACE`, `ESC`, and non-ASCII Unicode graphic characters.
pub(crate) fn format_mpv_keypress(
    key: &str,
    control: bool,
    shift: bool,
    alt: bool,
    meta: bool,
) -> Option<String> {
    if is_bare_modifier_key(key) {
        return None;
    }
    if let Some(base) = translate_mpv_key_name(key) {
        let mut name = String::new();
        if shift {
            name.push_str("Shift+");
        }
        if control {
            name.push_str("Ctrl+");
        }
        if alt {
            name.push_str("Alt+");
        }
        if meta {
            name.push_str("Meta+");
        }
        name.push_str(base);
        return Some(name);
    }
    if control && key.len() == 1 && matches!(key.as_bytes()[0], 0x01..=0x1a) {
        let letter = (b'a' + (key.as_bytes()[0] - 1)) as char;
        let mut name = String::new();
        if shift {
            name.push_str("Shift+");
        }
        name.push_str("Ctrl+");
        if alt {
            name.push_str("Alt+");
        }
        if meta {
            name.push_str("Meta+");
        }
        name.push(letter);
        return Some(name);
    }
    if key.chars().count() == 1 {
        let c = key.chars().next()?;
        let is_pua = ('\u{E000}'..='\u{F8FF}').contains(&c) || c >= '\u{F0000}';
        if c.is_control() || c.is_whitespace() || is_pua {
            return None;
        }
        let mut name = String::new();
        let has_non_shift_mod = control || alt || meta;
        let emit_char = if shift && c.is_ascii_lowercase() && !has_non_shift_mod {
            c.to_ascii_uppercase()
        } else {
            if shift && has_non_shift_mod && c.is_ascii_lowercase() {
                name.push_str("Shift+");
            }
            c
        };
        if control {
            name.push_str("Ctrl+");
        }
        if alt {
            name.push_str("Alt+");
        }
        if meta {
            name.push_str("Meta+");
        }
        name.push(emit_char);
        return Some(name);
    }
    None
}

pub(crate) fn keyboard_chord_for_os(
    key: &str,
    control: bool,
    shift: bool,
    alt: bool,
    meta: bool,
    is_macos: bool,
) -> String {
    if is_bare_modifier_key(key) {
        return String::new();
    }
    let mut chord = String::new();
    if control {
        chord.push_str("Ctrl+");
    }
    if is_macos && meta {
        chord.push_str("Cmd+");
    }
    if alt {
        chord.push_str("Alt+");
    }
    if shift {
        chord.push_str("Shift+");
    }
    if !is_macos && meta {
        chord.push_str("Meta+");
    }
    if let Some(named) = translate_slint_key_name(key) {
        chord.push_str(named);
    } else if control && key.len() == 1 && matches!(key.as_bytes()[0], 0x01..=0x1a) {
        let letter = (b'A' + (key.as_bytes()[0] - 1)) as char;
        chord.push(letter);
    } else {
        chord.push_str(&key.to_uppercase());
    }
    chord
}

#[cfg(test)]
pub(crate) fn keyboard_chord(
    key: &str,
    control: bool,
    shift: bool,
    alt: bool,
    meta: bool,
) -> String {
    keyboard_chord_for_os(key, control, shift, alt, meta, cfg!(target_os = "macos"))
}

fn canonical_chord_parts(chord: &str, is_macos: bool) -> Option<(bool, bool, bool, bool, String)> {
    let trimmed = chord.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut ctrl = false;
    let mut alt = false;
    let mut shift = false;
    let mut meta = false;
    let mut key = String::new();
    let tokens: Vec<&str> = trimmed.split('+').collect();
    for (idx, raw_tok) in tokens.iter().enumerate() {
        let tok = raw_tok.trim();
        let is_last = idx + 1 == tokens.len();
        if !is_last {
            match tok.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => {
                    ctrl = true;
                    continue;
                }
                "alt" | "option" | "opt" => {
                    alt = true;
                    continue;
                }
                "shift" => {
                    shift = true;
                    continue;
                }
                "cmd" | "command" | "meta" | "super" => {
                    if is_macos || !matches!(tok.to_ascii_lowercase().as_str(), "cmd" | "command") {
                        meta = true;
                        continue;
                    }
                    meta = true;
                    continue;
                }
                _ => {}
            }
        }
        if !tok.is_empty() {
            key = tok.to_ascii_uppercase();
        }
    }
    if key.is_empty() && trimmed.ends_with('+') {
        key = "+".to_owned();
    }
    (!key.is_empty()).then_some((ctrl, alt, shift, meta, key))
}

pub(crate) fn chords_match_for_os(actual: &str, binding: &str, is_macos: bool) -> bool {
    match (
        canonical_chord_parts(actual, is_macos),
        canonical_chord_parts(binding, is_macos),
    ) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

fn active_profile_keybind(config: &ConfigController, key: &str, default_value: &str) -> String {
    let profile = config.current_keybind_profile();
    let group = format!(
        "{}/{}/{}",
        raw::keybind::GROUP,
        raw::keybind::profile::GROUP,
        profile
    );
    match config.effective_setting(&group, key) {
        Some(Value::Text(text)) => text,
        _ => default_value.to_owned(),
    }
}

/// Estimate rendered row height in `YomitanPopup` for deterministic term-group
/// scrolling (`popup-target-scroll-y`) and viewport focus sync (`H4.1`).
pub(crate) fn estimate_popup_row_height(row: &DictResult) -> f32 {
    if row.collapsed && !row.term_start {
        return 0.0;
    }
    let header_h = if row.term_start { 42.0 } else { 0.0 };
    if row.collapsed && row.term_start {
        return header_h + 10.0;
    }
    let glossary_lines = row.glossary.as_str().lines().count().max(1) as f32;
    let body_h = (28.0 + glossary_lines * 19.0).clamp(48.0, 240.0);
    let spacing = if row.term_end { 10.0 } else { 0.0 };
    header_h + body_h + spacing
}

pub(crate) fn popup_term_start_indices(rows: &[DictResult]) -> Vec<usize> {
    let starts: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter_map(|(idx, row)| row.term_start.then_some(idx))
        .collect();
    if starts.is_empty() {
        (0..rows.len()).collect()
    } else {
        starts
    }
}

/// Step `current_focused` by one whole term group (`term_start == true`) in
/// `direction` (`> 0` next, `< 0` previous) and compute its target scroll y offset.
pub(crate) fn step_popup_term_group(
    rows: &[DictResult],
    current_focused: i32,
    direction: i32,
) -> Option<(usize, f32)> {
    if rows.is_empty() {
        return None;
    }
    let starts = popup_term_start_indices(rows);
    let cur = current_focused.max(0) as usize;
    let target = if direction > 0 {
        starts
            .iter()
            .copied()
            .find(|&idx| idx > cur)
            .unwrap_or(*starts.last().unwrap())
    } else if direction < 0 {
        starts
            .iter()
            .rev()
            .copied()
            .find(|&idx| idx < cur)
            .unwrap_or(*starts.first().unwrap())
    } else {
        starts
            .iter()
            .rev()
            .copied()
            .find(|&idx| idx <= cur)
            .unwrap_or(*starts.first().unwrap())
    };
    let scroll_y: f32 = rows[..target].iter().map(estimate_popup_row_height).sum();
    Some((target, scroll_y))
}

/// Resolve the top visible term-group index for a given positive popup scroll offset.
pub(crate) fn popup_top_term_group_at_scroll(rows: &[DictResult], scroll_y: f32) -> Option<usize> {
    if rows.is_empty() {
        return None;
    }
    let starts = popup_term_start_indices(rows);
    let clamped_scroll = scroll_y.max(0.0);
    if clamped_scroll <= 1.0 {
        return starts.first().copied();
    }
    let mut y = 0.0f32;
    let mut best = *starts.first().unwrap();
    for (idx, row) in rows.iter().enumerate() {
        if starts.contains(&idx) {
            if y <= clamped_scroll + 24.0 {
                best = idx;
            } else {
                break;
            }
        }
        y += estimate_popup_row_height(row);
    }
    Some(best)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn install_keybind_handlers(
    app: &MementoApp,
    config_ctrl: Rc<ConfigController>,
    mpv: Rc<RefCell<Option<MpvInstance>>>,
    closing: Rc<Cell<bool>>,
    hover_cursor: Rc<RefCell<Option<(String, i32)>>>,
    audio_context_marks: Rc<RefCell<AudioContextMarkState>>,
) {
    // H4.1: Override popup navigation to step strictly by term group (`term_start == true`)
    // and keep `popup-target-scroll-y` and `popup-viewport-scrolled` synchronized.
    let last_popup_scroll_y = Rc::new(Cell::new(0.0f32));
    let nav_app = app.as_weak();
    let nav_last_scroll = last_popup_scroll_y.clone();
    app.on_navigate_popup(move |direction| {
        let Some(app) = nav_app.upgrade() else {
            return;
        };
        let collect_rows = |app: &MementoApp| -> Vec<DictResult> {
            let model = app.get_popup_results();
            (0..model.row_count())
                .filter_map(|i| model.row_data(i))
                .collect()
        };
        let mut rows = collect_rows(&app);
        if direction > 0 {
            let starts = popup_term_start_indices(&rows);
            let cur = app.get_popup_focused_index().max(0) as usize;
            let at_last_loaded_group = starts.last().is_none_or(|&last| cur >= last);
            if at_last_loaded_group
                && (app.get_popup_loaded_count() as usize) < app.get_dict_results().row_count()
            {
                app.invoke_load_more_definition_results(true);
                rows = collect_rows(&app);
            }
        }
        if let Some((target_idx, scroll_y)) =
            step_popup_term_group(&rows, app.get_popup_focused_index(), direction)
        {
            app.set_popup_focused_index(target_idx as i32);
            // Force Slint's `changed target-scroll-y` to fire if manual scrolling drifted
            // away while `target-scroll-y` still held the previous numeric value.
            if (app.get_popup_target_scroll_y() - scroll_y).abs() < 0.01
                && (nav_last_scroll.get() - scroll_y).abs() > 0.5
            {
                app.set_popup_target_scroll_y(scroll_y + 0.5);
            }
            app.set_popup_target_scroll_y(scroll_y);
            nav_last_scroll.set(scroll_y);
        }
        if direction > 0
            && (app.get_popup_loaded_count() as usize) < app.get_dict_results().row_count()
        {
            let starts = popup_term_start_indices(&rows);
            let cur = app.get_popup_focused_index().max(0) as usize;
            if starts.last().is_some_and(|&last| cur >= last) {
                app.invoke_load_more_definition_results(true);
            }
        }
    });

    let scroll_app = app.as_weak();
    let scroll_last = last_popup_scroll_y;
    app.on_popup_viewport_scrolled(move |scroll_y| {
        scroll_last.set(scroll_y);
        let Some(app) = scroll_app.upgrade() else {
            return;
        };
        let model = app.get_popup_results();
        let rows: Vec<DictResult> = (0..model.row_count())
            .filter_map(|i| model.row_data(i))
            .collect();
        if let Some(top_idx) = popup_top_term_group_at_scroll(&rows, scroll_y) {
            if app.get_popup_focused_index() != top_idx as i32 {
                app.set_popup_focused_index(top_idx as i32);
            }
        }
    });

    let key_config = config_ctrl;
    let key_mpv = mpv;
    let key_closing = closing.clone();
    let key_hover_cursor = hover_cursor;
    let key_audio_marks = audio_context_marks;
    let shortcut_app = app.as_weak();
    app.set_custom_key_handler_installed(true);
    app.on_key_pressed(move |text, control, shift, alt, meta| {
        if key_closing.get() {
            return true;
        }
        let Some(app) = shortcut_app.upgrade() else {
            return false;
        };
        // Bare modifiers (Shift/Ctrl/Alt/Super) must never dismiss YomitanPopup,
        // and pressing a lookup modifier while hovering a glossary word or subtitle triggers lookup.
        if is_bare_modifier_key(&text) {
            let (mc, ms, ma, mm) = bare_modifier_flags(&text);
            let c = control || mc;
            let s = shift || ms;
            let a = alt || ma;
            let m = meta || mm;
            if app.get_dict_open()
                && app
                    .global::<PopupBridge>()
                    .invoke_popup_key_pressed(text.clone(), c, s, a, m)
            {
                return true;
            }
            if app.global::<SubtitleOverlayBridge>().get_rich_enabled() {
                app.global::<SubtitleOverlayBridge>()
                    .invoke_stationary_modifier_changed(c, s, a, m);
            } else if c || s || a || m {
                let hovered = key_hover_cursor.borrow().clone();
                if let Some((word, index)) = hovered {
                    app.invoke_hover_word(word.into(), index, c, s, a, m);
                }
            }
            return false;
        }
        let is_macos = cfg!(target_os = "macos");
        let chord = keyboard_chord_for_os(&text, control, shift, alt, meta, is_macos);
        let assigned_str = |binding: &str| {
            !binding.trim().is_empty() && chords_match_for_os(&chord, binding, is_macos)
        };
        let assigned = |binding: &slint::SharedString| assigned_str(binding.as_str());

        // Popup navigation, Anki card-add (routing to active nested popup when open),
        // and Escape on nested popups operate on the open YomitanPopup without dismissing it.
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
                let bridge = app.global::<PopupBridge>();
                if bridge.get_rich_enabled() && bridge.get_nested_depth() > 0 {
                    bridge.invoke_nested_add_anki_note(0);
                } else {
                    let idx = app.get_popup_focused_index().max(0);
                    app.invoke_add_anki_note(idx);
                }
                return true;
            }
            if text == "\u{001b}" || text == "Escape" {
                let bridge = app.global::<PopupBridge>();
                if bridge.get_rich_enabled() && bridge.get_nested_depth() > 0 {
                    bridge.invoke_pop_nested_popup();
                    return true;
                }
            }
        }

        // H5.2 / #136: Manual A/B audio context marking & clearing (works even while popup is open).
        let default_mark = if is_macos { "Cmd+B" } else { "Ctrl+B" };
        let default_clear = if is_macos {
            "Cmd+Shift+B"
        } else {
            "Ctrl+Shift+B"
        };
        let mark_shortcut =
            active_profile_keybind(&key_config, KEYBIND_MARK_AUDIO_CONTEXT, default_mark);
        let clear_shortcut =
            active_profile_keybind(&key_config, KEYBIND_CLEAR_AUDIO_CONTEXT, default_clear);
        if assigned_str(&clear_shortcut) {
            app.invoke_clear_audio_context();
            return true;
        }
        if assigned_str(&mark_shortcut) {
            let pos_sec = key_mpv
                .borrow()
                .as_ref()
                .map(|player| player.playback_state().position)
                .unwrap_or_else(|| f64::from(app.get_position()));
            let hud_text = key_audio_marks.borrow_mut().mark_at(pos_sec);
            app.set_audio_context_hud_text(hud_text.into());
            return true;
        }

        // Any other keyboard input dismisses a definition, even when the key is
        // handled by the player or a child widget.
        let had_dict_open = app.get_dict_open();
        app.invoke_dismiss_definition();
        let settings_shortcut = if is_macos {
            SETTINGS_SHORTCUT_MACOS
        } else {
            SETTINGS_SHORTCUT_DEFAULT
        };
        let is_settings_shortcut = assigned_str(settings_shortcut)
            || (is_macos && meta && !control && !alt && (text == "," || text == "<"))
            || (!is_macos && control && !meta && !alt && (text == "," || text == "<"));
        if assigned(&app.get_key_open_file()) {
            app.invoke_open_file();
        } else if assigned(&app.get_key_open_url()) {
            app.invoke_open_url();
        } else if assigned(&app.get_key_quit()) {
            app.invoke_quit_app();
        } else if assigned(&app.get_key_show_search()) {
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
        } else if is_settings_shortcut {
            app.invoke_open_options();
        } else if text == "\u{001b}" || text == "Escape" {
            if app.get_update_dialog_open() {
                if app.get_update_dialog_opt_in() {
                    app.invoke_update_opt_in_responded(false);
                } else {
                    app.set_update_dialog_open(false);
                    app.invoke_update_dialog_dismissed();
                }
            } else if app.get_no_dicts_dialog_open() {
                app.set_no_dicts_dialog_open(false);
            } else if app.get_url_dialog_open() {
                app.set_url_dialog_open(false);
            } else if app.get_about_open() {
                app.set_about_open(false);
            } else if app.get_ocr_mode() {
                app.set_ocr_mode(false);
                app.invoke_ocr_cancelled();
            } else if had_dict_open {
            } else if app.get_dict_search_open() {
                app.set_dict_search_open(false);
            } else if app.get_sub_drawer_open() {
                app.set_sub_drawer_open(false);
            } else if app.get_is_fullscreen() {
                app.invoke_toggle_fullscreen();
            } else if let Some(key_name) =
                format_mpv_keypress(text.as_str(), control, shift, alt, meta)
            {
                // H3.3: Forward Escape to mpv when no Memento modal/popup/OCR/fullscreen consumed it.
                if let Some(player) = &*key_mpv.borrow() {
                    let _ = player.command(&["keypress", &key_name]);
                    return true;
                }
                return false;
            } else {
                return false;
            }
        } else {
            // H3.3: Forward all non-Memento keys (including Space, f/F, q/Q, F13..F30,
            // PAUSE, KP_*, modified Space, and non-ASCII graphic keys) directly to mpv.
            if let Some(key_name) = format_mpv_keypress(text.as_str(), control, shift, alt, meta) {
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
    let key_rel_closing = closing;
    app.on_key_released(move |control, shift, alt, meta| {
        if key_rel_closing.get() {
            return;
        }
        let Some(app) = key_rel_app.upgrade() else {
            return;
        };
        if app.get_dict_open() {
            app.global::<PopupBridge>()
                .invoke_popup_key_released(control, shift, alt, meta);
            if app.global::<PopupBridge>().get_rich_enabled() {
                return;
            }
        }
        if app.global::<SubtitleOverlayBridge>().get_rich_enabled() {
            app.global::<SubtitleOverlayBridge>()
                .invoke_stationary_modifier_changed(control, shift, alt, meta);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::subtitle_drawer::{install_subtitle_selection, refresh_subtitle_list};
    use crate::controller::player::refresh_subtitle_marks;
    use crate::controller::subtitles::{create_cue, SubtitleController};
    use crate::ui::{set_dictionary_rows, DictResult, SubEntry};
    use slint::platform::{Key, WindowEvent};
    use slint::VecModel;

    #[test]
    fn slint_pua_keys_and_bare_modifiers_translate_cleanly_in_keyboard_chord() {
        assert_eq!(
            keyboard_chord("\u{F700}", true, false, false, false),
            "Ctrl+Up"
        );
        assert_eq!(
            keyboard_chord("\u{F701}", true, false, false, false),
            "Ctrl+Down"
        );
        assert_eq!(
            keyboard_chord("\u{F702}", false, false, true, false),
            "Alt+Left"
        );
        assert_eq!(
            keyboard_chord("\u{F703}", false, false, true, false),
            "Alt+Right"
        );
        assert_eq!(keyboard_chord("\u{F704}", false, false, false, false), "F1");
        assert_eq!(keyboard_chord("\u{F706}", false, false, false, false), "F3");
        assert_eq!(
            keyboard_chord("\u{F706}", false, true, false, false),
            "Shift+F3"
        );
        assert_eq!(
            keyboard_chord("\u{F70F}", false, false, false, false),
            "F12"
        );
        assert_eq!(
            keyboard_chord("\u{F727}", false, false, false, false),
            "Insert"
        );
        assert_eq!(
            keyboard_chord("\u{007f}", false, false, false, false),
            "Delete"
        );
        assert_eq!(
            keyboard_chord("\u{F728}", false, false, false, false),
            "Delete"
        );
        assert_eq!(
            keyboard_chord("\u{F729}", false, false, false, false),
            "Home"
        );
        assert_eq!(
            keyboard_chord("\u{F72B}", false, false, false, false),
            "End"
        );
        assert_eq!(
            keyboard_chord("\u{F72C}", false, false, false, false),
            "PageUp"
        );
        assert_eq!(
            keyboard_chord("\u{F72D}", false, false, false, false),
            "PageDown"
        );
        assert_eq!(
            keyboard_chord("\u{F729}", true, false, false, false),
            "Ctrl+Home"
        );
        assert_eq!(
            keyboard_chord("\u{F72C}", false, true, false, false),
            "Shift+PageUp"
        );
        assert_eq!(
            keyboard_chord("\u{001b}", false, false, false, false),
            "Escape"
        );
        assert_eq!(
            keyboard_chord("\u{0008}", false, false, false, false),
            "Backspace"
        );
        assert_eq!(
            keyboard_chord("\u{0009}", false, false, false, false),
            "Tab"
        );
        assert_eq!(
            keyboard_chord("\u{000a}", false, false, false, false),
            "Return"
        );
        assert_eq!(
            keyboard_chord("c", true, true, false, false),
            "Ctrl+Shift+C"
        );
        assert_eq!(keyboard_chord("d", true, false, false, false), "Ctrl+D");

        for bare_mod in [
            "", "\u{0010}", "\u{0011}", "\u{0012}", "\u{0013}", "\u{0014}", "\u{0015}", "\u{0016}",
            "\u{0017}", "\u{0018}", "Shift", "Control", "Ctrl", "Alt", "Meta", "Super",
        ] {
            assert!(is_bare_modifier_key(bare_mod));
            assert_eq!(keyboard_chord(bare_mod, true, true, true, true), "");
        }
        assert_eq!(
            bare_modifier_flags(&String::from(char::from(Key::Shift))),
            (false, true, false, false)
        );
        assert_eq!(
            bare_modifier_flags(&String::from(char::from(Key::ShiftR))),
            (false, true, false, false)
        );
        assert_eq!(
            bare_modifier_flags(&String::from(char::from(Key::Control))),
            (true, false, false, false)
        );
        assert_eq!(
            bare_modifier_flags(&String::from(char::from(Key::ControlR))),
            (true, false, false, false)
        );
        assert_eq!(
            bare_modifier_flags(&String::from(char::from(Key::Alt))),
            (false, false, true, false)
        );
        assert_eq!(
            bare_modifier_flags(&String::from(char::from(Key::AltGr))),
            (false, false, true, false)
        );
        assert_eq!(
            bare_modifier_flags(&String::from(char::from(Key::Meta))),
            (false, false, false, true)
        );
        assert_eq!(
            bare_modifier_flags(&String::from(char::from(Key::MetaR))),
            (false, false, false, true)
        );
        assert_eq!(
            bare_modifier_flags(&String::from(char::from(Key::CapsLock))),
            (false, false, false, false)
        );
        assert_eq!(bare_modifier_flags("Shift"), (false, true, false, false));
        assert_eq!(bare_modifier_flags("ShiftR"), (false, true, false, false));
        assert_eq!(bare_modifier_flags("Control"), (true, false, false, false));
        assert_eq!(bare_modifier_flags("Ctrl"), (true, false, false, false));
        assert_eq!(bare_modifier_flags("Alt"), (false, false, true, false));
        assert_eq!(bare_modifier_flags("Meta"), (false, false, false, true));
        assert_eq!(bare_modifier_flags("Super"), (false, false, false, true));
    }

    #[test]
    fn mpv_keypress_formatting_macos_cmd_no_preinterception_popup_nav_and_ab_audio_context() {
        // H1.3: macOS Cmd mapping & canonical chord matching
        assert_eq!(
            keyboard_chord_for_os("b", false, true, false, true, true),
            "Cmd+Shift+B"
        );
        assert_eq!(
            keyboard_chord_for_os(",", false, false, false, true, true),
            "Cmd+,"
        );
        assert!(chords_match_for_os("Cmd+Shift+C", "Shift+Meta+C", true));

        // H3.3: Complete mpv key mapping & Shift+Ctrl+Alt+Meta+ ordering
        assert_eq!(
            format_mpv_keypress(" ", false, false, false, false).as_deref(),
            Some("SPACE")
        );
        assert_eq!(
            format_mpv_keypress(" ", true, true, true, true).as_deref(),
            Some("Shift+Ctrl+Alt+Meta+SPACE")
        );
        assert_eq!(
            format_mpv_keypress("\u{F710}", false, false, false, false).as_deref(),
            Some("F13")
        );
        assert_eq!(
            format_mpv_keypress("\u{F721}", true, true, false, false).as_deref(),
            Some("Shift+Ctrl+F30")
        );
        assert_eq!(
            format_mpv_keypress("\u{F730}", false, false, false, false).as_deref(),
            Some("PAUSE")
        );
        assert_eq!(
            format_mpv_keypress("KP_ENTER", true, true, true, false).as_deref(),
            Some("Shift+Ctrl+Alt+KP_ENTER")
        );
        assert_eq!(
            format_mpv_keypress("\u{001b}", false, false, false, false).as_deref(),
            Some("ESC")
        );
        assert_eq!(
            format_mpv_keypress("ö", false, false, false, false).as_deref(),
            Some("ö")
        );
        assert_eq!(
            format_mpv_keypress("あ", true, false, false, false).as_deref(),
            Some("Ctrl+あ")
        );
        assert_eq!(
            format_mpv_keypress("q", false, false, false, false).as_deref(),
            Some("q")
        );
        assert_eq!(
            format_mpv_keypress("Q", false, true, false, false).as_deref(),
            Some("Q")
        );
        assert_eq!(format_mpv_keypress("\u{E001}", false, false, false, false), None);

        // H5.2: Deterministic A/B audio context state machine & timestamp formatting
        assert_eq!(format_precise_ts(12.34), "00:12.34");
        assert_eq!(format_precise_ts(3661.5), "01:01:01.50");
        let mut marks = AudioContextMarkState::default();
        assert_eq!(marks.mark_at(20.5), "Audio Context: 00:20.50 → ...");
        // Auto-swap when B < A
        assert_eq!(marks.mark_at(10.25), "Audio Context: 00:10.25 → 00:20.50");
        // Third press starts a new A mark
        assert_eq!(marks.mark_at(45.0), "Audio Context: 00:45.00 → ...");
        marks.clear();
        assert_eq!(marks, AudioContextMarkState::Empty);

        i_slint_backend_testing::init_no_event_loop();
        let app = MementoApp::new().unwrap();
        let cfg = Rc::new(ConfigController::new());
        let mpv: Rc<RefCell<Option<MpvInstance>>> = Rc::new(RefCell::new(None));
        let closing = Rc::new(Cell::new(false));
        let hover = Rc::new(RefCell::new(None));
        let audio_marks = Rc::new(RefCell::new(AudioContextMarkState::default()));
        install_keybind_handlers(
            &app,
            cfg,
            mpv,
            closing,
            hover,
            audio_marks.clone(),
        );

        let quit_count = Rc::new(Cell::new(0usize));
        let quit_sink = quit_count.clone();
        app.on_quit_app(move || quit_sink.set(quit_sink.get() + 1));
        let play_count = Rc::new(Cell::new(0usize));
        let play_sink = play_count.clone();
        app.on_toggle_play(move || play_sink.set(play_sink.get() + 1));
        let fs_count = Rc::new(Cell::new(0usize));
        let fs_sink = fs_count.clone();
        app.on_toggle_fullscreen(move || fs_sink.set(fs_sink.get() + 1));
        let clear_marks = audio_marks.clone();
        let clear_app = app.as_weak();
        app.on_clear_audio_context(move || {
            clear_marks.borrow_mut().clear();
            if let Some(app) = clear_app.upgrade() {
                app.set_audio_context_hud_text("".into());
            }
        });

        // H3.3: Space, f/F, q/Q are NOT pre-intercepted before mpv.
        app.invoke_key_pressed(" ".into(), false, false, false, false);
        app.invoke_key_pressed("f".into(), false, false, false, false);
        app.invoke_key_pressed("q".into(), false, false, false, false);
        app.invoke_key_pressed("Q".into(), false, true, false, false);
        assert_eq!(play_count.get(), 0);
        assert_eq!(fs_count.get(), 0);
        assert_eq!(quit_count.get(), 0);

        // App Quit keybind (Ctrl+Q / Cmd+Q) quits cleanly.
        app.invoke_key_pressed(
            "q".into(),
            !cfg!(target_os = "macos"),
            false,
            false,
            cfg!(target_os = "macos"),
        );
        assert_eq!(quit_count.get(), 1);

        // `key_card_add` (`Ctrl+D`) must NOT toggle search panel when popup is closed;
        // only `key_show_search` (`Ctrl+R`) toggles it.
        assert!(!app.get_dict_open());
        assert!(!app.get_dict_search_open());
        app.invoke_key_pressed("d".into(), true, false, false, false);
        assert!(!app.get_dict_search_open());
        app.invoke_key_pressed("r".into(), true, false, false, false);
        assert!(app.get_dict_search_open());

        // H5.2: MarkAudioContext / ClearAudioContext keybinds update HUD text cleanly.
        app.set_position(12.5);
        app.invoke_key_pressed(
            "b".into(),
            !cfg!(target_os = "macos"),
            false,
            false,
            cfg!(target_os = "macos"),
        );
        assert_eq!(
            app.get_audio_context_hud_text().as_str(),
            "Audio Context: 00:12.50 → ..."
        );
        app.set_position(25.0);
        app.invoke_key_pressed(
            "b".into(),
            !cfg!(target_os = "macos"),
            false,
            false,
            cfg!(target_os = "macos"),
        );
        assert_eq!(
            app.get_audio_context_hud_text().as_str(),
            "Audio Context: 00:12.50 → 00:25.00"
        );
        app.invoke_key_pressed(
            "b".into(),
            !cfg!(target_os = "macos"),
            true,
            false,
            cfg!(target_os = "macos"),
        );
        assert_eq!(app.get_audio_context_hud_text().as_str(), "");

        // H4.1: Popup navigation steps by term_start groups, sets target_scroll_y,
        // syncs on viewport_scrolled, and routes Ctrl+D to nested popup when open.
        set_dictionary_rows(
            &app,
            vec![
                DictResult {
                    index: 1,
                    expression: "食べる".into(),
                    reading: "たべる".into(),
                    glossary: "to eat".into(),
                    dict_name: "JMdict".into(),
                    term_start: true,
                    term_end: false,
                    ..Default::default()
                },
                DictResult {
                    index: 2,
                    expression: "食べる".into(),
                    reading: "たべる".into(),
                    glossary: "second definition".into(),
                    dict_name: "Daijirin".into(),
                    term_start: false,
                    term_end: true,
                    ..Default::default()
                },
                DictResult {
                    index: 3,
                    expression: "飲む".into(),
                    reading: "のむ".into(),
                    glossary: "to drink".into(),
                    dict_name: "JMdict".into(),
                    term_start: true,
                    term_end: true,
                    ..Default::default()
                },
            ],
        );
        app.set_dict_open(true);
        app.set_popup_focused_index(0);
        app.invoke_navigate_popup(1);
        assert_eq!(
            app.get_popup_focused_index(),
            2,
            "Ctrl+Down must jump to next term_start group, skipping non-term_start rows"
        );
        assert!(app.get_popup_target_scroll_y() > 0.0);
        app.invoke_popup_viewport_scrolled(0.0);
        assert_eq!(app.get_popup_focused_index(), 0);
        app.invoke_navigate_popup(1);
        assert_eq!(app.get_popup_focused_index(), 2);
        app.invoke_navigate_popup(-1);
        assert_eq!(app.get_popup_focused_index(), 0);
        assert_eq!(app.get_popup_target_scroll_y(), 0.0);

        // Nested Ctrl+D routes to PopupBridge.nested-add-anki-note instead of root add-anki-note.
        app.set_anki_enabled(true);
        let root_added = Rc::new(Cell::new(-1));
        let root_sink = root_added.clone();
        app.on_add_anki_note(move |idx| root_sink.set(idx));
        let nested_added = Rc::new(Cell::new(-1));
        let nested_sink = nested_added.clone();
        let bridge = app.global::<PopupBridge>();
        bridge.set_rich_enabled(true);
        bridge.set_nested_depth(1);
        bridge.on_nested_add_anki_note(move |idx| nested_sink.set(idx));
        app.invoke_key_pressed(
            "d".into(),
            !cfg!(target_os = "macos"),
            false,
            false,
            cfg!(target_os = "macos"),
        );
        assert_eq!(nested_added.get(), 0);
        assert_eq!(root_added.get(), -1);
    }

    #[test]
    fn bare_modifiers_and_popup_shortcuts_preserve_yomitan_popup_and_all_eight_keybinds_work() {
        i_slint_backend_testing::init_no_event_loop();
        let app = MementoApp::new().unwrap();
        app.window().set_size(slint::LogicalSize::new(1280., 720.));
        let subtitles = Rc::new(RefCell::new(SubtitleController::new()));
        subtitles.borrow_mut().load_cues(vec![
            create_cue("First match line", 1., 2.),
            create_cue("Second middle line", 3., 4.),
            create_cue("Third match line", 5., 6.),
        ]);
        let rows = Rc::new(VecModel::<SubEntry>::default());
        app.set_subtitle_entries(rows.clone().into());
        refresh_subtitle_list(&app, &rows, &subtitles.borrow());
        install_subtitle_selection(&app, subtitles.clone(), rows.clone());
        app.show().unwrap();

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
            app.window()
                .dispatch_event(WindowEvent::KeyPressed { text: key.into() });
            app.window()
                .dispatch_event(WindowEvent::KeyReleased { text: key.into() });
            assert!(
                app.get_dict_open(),
                "bare modifier {key:?} must not dismiss YomitanPopup"
            );
        }

        let navigated = Rc::new(Cell::new(0));
        let nav_sink = navigated.clone();
        app.on_navigate_popup(move |dir| nav_sink.set(dir));
        app.window().dispatch_event(WindowEvent::KeyPressed {
            text: Key::Control.into(),
        });
        app.window().dispatch_event(WindowEvent::KeyPressed {
            text: Key::DownArrow.into(),
        });
        app.window().dispatch_event(WindowEvent::KeyReleased {
            text: Key::DownArrow.into(),
        });
        assert!(app.get_dict_open(), "Ctrl+Down must keep YomitanPopup open");
        assert_eq!(navigated.get(), 1);
        app.window().dispatch_event(WindowEvent::KeyPressed {
            text: Key::UpArrow.into(),
        });
        app.window().dispatch_event(WindowEvent::KeyReleased {
            text: Key::UpArrow.into(),
        });
        app.window().dispatch_event(WindowEvent::KeyReleased {
            text: Key::Control.into(),
        });
        assert!(app.get_dict_open(), "Ctrl+Up must keep YomitanPopup open");
        assert_eq!(navigated.get(), -1);

        app.set_anki_enabled(true);
        let added_note = Rc::new(Cell::new(-1));
        let added_sink = added_note.clone();
        app.on_add_anki_note(move |idx| added_sink.set(idx));
        app.window().dispatch_event(WindowEvent::KeyPressed {
            text: Key::Control.into(),
        });
        app.window()
            .dispatch_event(WindowEvent::KeyPressed { text: "d".into() });
        app.window()
            .dispatch_event(WindowEvent::KeyReleased { text: "d".into() });
        app.window().dispatch_event(WindowEvent::KeyReleased {
            text: Key::Control.into(),
        });
        assert!(app.get_dict_open());
        assert_eq!(added_note.get(), 0);

        app.invoke_dismiss_definition();
        assert!(!app.get_dict_open());
        assert!(!app.get_dict_search_open());
        app.window().dispatch_event(WindowEvent::KeyPressed {
            text: Key::Control.into(),
        });
        app.window()
            .dispatch_event(WindowEvent::KeyPressed { text: "d".into() });
        app.window()
            .dispatch_event(WindowEvent::KeyReleased { text: "d".into() });
        app.window().dispatch_event(WindowEvent::KeyReleased {
            text: Key::Control.into(),
        });
        assert!(
            app.get_dict_search_open(),
            "Ctrl+D must toggle dictionary search panel"
        );

        assert_eq!(app.get_osc_visibility_mode(), 0);
        app.window().dispatch_event(WindowEvent::KeyPressed {
            text: Key::Delete.into(),
        });
        app.window().dispatch_event(WindowEvent::KeyReleased {
            text: Key::Delete.into(),
        });
        assert_eq!(app.get_osc_visibility_mode(), 1);
        assert!(app.get_osc_visible());
        app.window().dispatch_event(WindowEvent::KeyPressed {
            text: Key::Delete.into(),
        });
        app.window().dispatch_event(WindowEvent::KeyReleased {
            text: Key::Delete.into(),
        });
        assert_eq!(app.get_osc_visibility_mode(), 2);
        assert!(!app.get_osc_visible());
        app.window().dispatch_event(WindowEvent::KeyPressed {
            text: Key::Delete.into(),
        });
        app.window().dispatch_event(WindowEvent::KeyReleased {
            text: Key::Delete.into(),
        });
        assert_eq!(app.get_osc_visibility_mode(), 0);

        app.set_sub_drawer_open(false);
        app.window().dispatch_event(WindowEvent::KeyPressed {
            text: Key::Control.into(),
        });
        app.window()
            .dispatch_event(WindowEvent::KeyPressed { text: "f".into() });
        app.window()
            .dispatch_event(WindowEvent::KeyReleased { text: "f".into() });
        app.window().dispatch_event(WindowEvent::KeyReleased {
            text: Key::Control.into(),
        });
        assert!(
            app.get_sub_drawer_open(),
            "Ctrl+F must open SubtitleDrawerPanel"
        );

        app.invoke_search_subtitles("match".into());
        assert_eq!(app.get_subtitle_search_total_matches(), 2);
        assert_eq!(app.get_subtitle_search_current_match(), 1);
        assert_eq!(app.get_subtitle_search_focused_row(), 0);
        app.window().dispatch_event(WindowEvent::KeyPressed {
            text: Key::F3.into(),
        });
        app.window().dispatch_event(WindowEvent::KeyReleased {
            text: Key::F3.into(),
        });
        assert_eq!(app.get_subtitle_search_current_match(), 2);
        assert_eq!(app.get_subtitle_search_focused_row(), 2);
        app.window().dispatch_event(WindowEvent::KeyPressed {
            text: Key::Shift.into(),
        });
        app.window().dispatch_event(WindowEvent::KeyPressed {
            text: Key::F3.into(),
        });
        app.window().dispatch_event(WindowEvent::KeyReleased {
            text: Key::F3.into(),
        });
        app.window().dispatch_event(WindowEvent::KeyReleased {
            text: Key::Shift.into(),
        });
        assert_eq!(app.get_subtitle_search_current_match(), 1);
        assert_eq!(app.get_subtitle_search_focused_row(), 0);

        subtitles.borrow_mut().select_row(1, false);
        refresh_subtitle_marks(&rows, &subtitles.borrow());
        app.window().dispatch_event(WindowEvent::KeyPressed {
            text: Key::Control.into(),
        });
        app.window().dispatch_event(WindowEvent::KeyPressed {
            text: Key::Shift.into(),
        });
        app.window()
            .dispatch_event(WindowEvent::KeyPressed { text: "C".into() });
        app.window()
            .dispatch_event(WindowEvent::KeyReleased { text: "C".into() });
        app.window().dispatch_event(WindowEvent::KeyReleased {
            text: Key::Shift.into(),
        });
        app.window().dispatch_event(WindowEvent::KeyReleased {
            text: Key::Control.into(),
        });
        assert_eq!(
            app.get_copied_subtitle_text().as_str(),
            "Second middle line"
        );
    }
}

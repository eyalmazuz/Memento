//! Keyboard shortcut translation (Slint PUA keys, bare modifiers, `input.conf` inspection)
//! and main-window key event dispatch.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use slint::ComponentHandle;

use crate::ui::{MementoApp, PopupBridge, SubtitleOverlayBridge};
use memento_player::MpvInstance;

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
        "\u{F727}" => Some("Insert"),
        "\u{007f}" | "\u{F728}" => Some("Delete"),
        "\u{F729}" => Some("Home"),
        "\u{F72B}" => Some("End"),
        "\u{F72C}" => Some("PageUp"),
        "\u{F72D}" => Some("PageDown"),
        "\u{001b}" => Some("Escape"),
        "\u{0008}" => Some("Backspace"),
        "\u{0009}" | "\u{0019}" => Some("Tab"),
        "\u{000a}" | "\r" => Some("Return"),
        " " => Some("Space"),
        _ => None,
    }
}

fn translate_mpv_key_name(key: &str) -> Option<&'static str> {
    match key {
        "\u{F700}" => Some("UP"),
        "\u{F701}" => Some("DOWN"),
        "\u{F702}" => Some("LEFT"),
        "\u{F703}" => Some("RIGHT"),
        "\u{0008}" => Some("BS"),
        "\u{000a}" | "\r" => Some("ENTER"),
        "\u{0009}" | "\u{0019}" => Some("TAB"),
        "\u{F727}" => Some("INS"),
        "\u{007f}" | "\u{F728}" => Some("DEL"),
        "\u{F729}" => Some("HOME"),
        "\u{F72B}" => Some("END"),
        "\u{F72C}" => Some("PGUP"),
        "\u{F72D}" => Some("PGDWN"),
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
    }
}

pub(crate) fn keyboard_chord(
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
    if control {
        chord.push_str("Ctrl+");
    }
    if alt {
        chord.push_str("Alt+");
    }
    if shift {
        chord.push_str("Shift+");
    }
    if meta {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InputConfQuitAction {
    /// No custom override (or explicit `quit` binding): quit Memento directly.
    DefaultQuit,
    /// `quit-watch-later` binding: persist watch-later config then quit Memento.
    QuitWatchLater,
    /// Custom non-quit binding (e.g. `q ignore`, `q seek -5`): forward to mpv.
    CustomBinding,
}

/// Inspect mpv's `input.conf` text for a case-sensitive binding of `key` (`"q"` or `"Q"`).
/// Last matching line wins, matching mpv's `input.conf` precedence.
pub(crate) fn parse_input_conf_quit_action(contents: &str, key: &str) -> InputConfQuitAction {
    let mut result = if key == "Q" {
        InputConfQuitAction::QuitWatchLater
    } else {
        InputConfQuitAction::DefaultQuit
    };
    for raw_line in contents.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(token) = parts.next() else {
            continue;
        };
        if token != key {
            continue;
        }
        let Some(cmd) = parts.next() else {
            continue;
        };
        if cmd.starts_with('#') {
            continue;
        }
        result = match cmd {
            "quit" => InputConfQuitAction::DefaultQuit,
            "quit-watch-later" => InputConfQuitAction::QuitWatchLater,
            _ => InputConfQuitAction::CustomBinding,
        };
    }
    result
}

pub(crate) fn inspect_input_conf_quit_action(config_dir: &Path, key: &str) -> InputConfQuitAction {
    match std::fs::read_to_string(config_dir.join("input.conf")) {
        Ok(contents) => parse_input_conf_quit_action(&contents, key),
        Err(_) => {
            if key == "Q" {
                InputConfQuitAction::QuitWatchLater
            } else {
                InputConfQuitAction::DefaultQuit
            }
        }
    }
}

pub(crate) fn install_keybind_handlers(
    app: &MementoApp,
    mpv: Rc<RefCell<Option<MpvInstance>>>,
    closing: Rc<Cell<bool>>,
    hover_cursor: Rc<RefCell<Option<(String, i32)>>>,
    config_dir: PathBuf,
) {
    let key_mpv = mpv;
    let key_closing = closing.clone();
    let key_hover_cursor = hover_cursor;
    let key_config_dir = config_dir;
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
        } else if !control && !alt && !meta && (text == "f" || text == "F") {
            app.invoke_toggle_fullscreen();
        } else if chord.eq_ignore_ascii_case("ctrl+,") || (control && (text == "," || text == "<"))
        {
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
            if !control && !alt && !meta && (text == "q" || text == "Q") {
                match inspect_input_conf_quit_action(&key_config_dir, text.as_str()) {
                    InputConfQuitAction::DefaultQuit => {
                        app.invoke_quit_app();
                        return true;
                    }
                    InputConfQuitAction::QuitWatchLater => {
                        if let Some(player) = key_mpv.borrow().as_ref() {
                            let _ = player.command(&["write-watch-later-config"]);
                        }
                        app.invoke_quit_app();
                        return true;
                    }
                    InputConfQuitAction::CustomBinding => {}
                }
            }
            let key_base = translate_mpv_key_name(text.as_str());
            let mpv_key = if let Some(base) = key_base {
                let mut name = String::new();
                if control {
                    name.push_str("Ctrl+");
                }
                if alt {
                    name.push_str("Alt+");
                }
                if shift {
                    name.push_str("Shift+");
                }
                if meta {
                    name.push_str("Meta+");
                }
                name.push_str(base);
                Some(name)
            } else if text.chars().count() == 1 {
                let c = text.chars().next().unwrap();
                if c.is_ascii_graphic() {
                    let mut name = String::new();
                    if control {
                        name.push_str("Ctrl+");
                    }
                    if alt {
                        name.push_str("Alt+");
                    }
                    if meta {
                        name.push_str("Meta+");
                    }
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
    fn input_conf_quit_overrides_and_meta_key_guard() {
        assert_eq!(
            parse_input_conf_quit_action("", "q"),
            InputConfQuitAction::DefaultQuit
        );
        assert_eq!(
            parse_input_conf_quit_action("", "Q"),
            InputConfQuitAction::QuitWatchLater
        );
        assert_eq!(
            parse_input_conf_quit_action("# q ignore\nq seek -5\n", "q"),
            InputConfQuitAction::CustomBinding
        );
        assert_eq!(
            parse_input_conf_quit_action("q ignore\nQ cycle pause\n", "Q"),
            InputConfQuitAction::CustomBinding
        );
        assert_eq!(
            parse_input_conf_quit_action("q quit-watch-later\n", "q"),
            InputConfQuitAction::QuitWatchLater
        );
        assert_eq!(
            parse_input_conf_quit_action("q ignore\nq quit\n", "q"),
            InputConfQuitAction::DefaultQuit
        );

        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("input.conf"), "q ignore\n").unwrap();
        let app = MementoApp::new().unwrap();
        let mpv: Rc<RefCell<Option<MpvInstance>>> = Rc::new(RefCell::new(None));
        let closing = Rc::new(Cell::new(false));
        let hover = Rc::new(RefCell::new(None));
        install_keybind_handlers(&app, mpv, closing, hover, dir.path().to_path_buf());

        let quit_count = Rc::new(Cell::new(0usize));
        let quit_sink = quit_count.clone();
        app.on_quit_app(move || quit_sink.set(quit_sink.get() + 1));

        // Custom `q ignore` in input.conf must NOT invoke quit_app.
        app.invoke_key_pressed("q".into(), false, false, false, false);
        assert_eq!(quit_count.get(), 0);

        // Meta+Q must NOT invoke quit_app even without a custom `Q` binding.
        app.invoke_key_pressed("Q".into(), false, false, false, true);
        assert_eq!(quit_count.get(), 0);

        // Bare `Q` without override invokes quit_app.
        app.invoke_key_pressed("Q".into(), false, false, false, false);
        assert_eq!(quit_count.get(), 1);

        // `key_card_add` (`Ctrl+D`) must NOT toggle search panel when popup is closed;
        // only `key_show_search` (`Ctrl+R`) toggles it.
        assert!(!app.get_dict_open());
        assert!(!app.get_dict_search_open());
        app.invoke_key_pressed("d".into(), true, false, false, false);
        assert!(!app.get_dict_search_open());
        app.invoke_key_pressed("r".into(), true, false, false, false);
        assert!(app.get_dict_search_open());
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

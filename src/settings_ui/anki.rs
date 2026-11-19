//! Anki settings callbacks: help window, note presets, guarded connection testing,
//! and timer-free async model field loading via `Weak::upgrade_in_event_loop`.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use slint::{ComponentHandle, VecModel};

use crate::anki_help;
use crate::controller::anki::AnkiController;
use crate::{AnkiHelpWindow, SettingsWindow};

use super::{refresh_category_settings, SettingsCategory, SettingsContext};

struct PendingAnkiFields {
    section: String,
    profile_name: String,
    model: String,
    result: Result<Vec<String>, String>,
}

pub(crate) fn wire(window: &SettingsWindow, ctx: &Rc<SettingsContext>) {
    // A native Help window on Wayland becomes visible when it is created.
    // Construct it lazily on first request.
    let help_window = Rc::new(RefCell::new(None::<AnkiHelpWindow>));
    window.on_anki_help_requested(move || {
        if help_window.borrow().is_none() {
            match AnkiHelpWindow::new() {
                Ok(win) => {
                    anki_help::populate(&win);
                    *help_window.borrow_mut() = Some(win);
                }
                Err(error) => {
                    eprintln!("Could not create Anki help: {error}");
                    return;
                }
            }
        }
        if let Some(win) = help_window.borrow().as_ref() {
            if let Err(error) = win.show() {
                eprintln!("Could not open Anki help: {error}");
            }
        }
    });

    let preset_window = window.as_weak();
    let preset_ctx = ctx.clone();
    window.on_apply_anki_preset(move |preset_name| {
        let Some(window) = preset_window.upgrade() else {
            return;
        };
        match preset_ctx.config.apply_anki_note_preset(&preset_name) {
            Ok(count) => {
                preset_ctx.mark_dirty(SettingsCategory::Anki);
                refresh_category_settings(&window, &preset_ctx.config, SettingsCategory::Anki.as_i32());
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

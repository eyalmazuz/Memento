//! Centralized clipboard helper with a persistent `ClipboardContext`,
//! panic safety (`catch_unwind`), and error-triggered context recreation
//! (preventing X11 clipboard wipe on drop and stale-display breakage on reconnect).

use std::sync::{Mutex, OnceLock};

use copypasta::{ClipboardContext, ClipboardProvider};

static CLIPBOARD_HOLDER: OnceLock<Mutex<Option<ClipboardContext>>> = OnceLock::new();
static FALLBACK_CLIPBOARD: OnceLock<Mutex<String>> = OnceLock::new();

fn clipboard_holder() -> &'static Mutex<Option<ClipboardContext>> {
    CLIPBOARD_HOLDER.get_or_init(|| Mutex::new(None))
}

fn fallback_clipboard() -> &'static Mutex<String> {
    FALLBACK_CLIPBOARD.get_or_init(|| Mutex::new(String::new()))
}

/// Copy `text` into the OS clipboard while keeping the underlying X11/Wayland
/// clipboard context alive across calls. On provider error (e.g. X11 reconnect),
/// drops and recreates the `ClipboardContext` and retries once.
pub fn copy_text_to_clipboard(text: &str) {
    if let Ok(mut fallback) = fallback_clipboard().lock() {
        *fallback = text.to_owned();
    }
    let owned = text.to_owned();
    let _ = std::panic::catch_unwind(move || {
        let mut guard = clipboard_holder()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if guard.is_none() {
            *guard = ClipboardContext::new().ok();
        }
        let succeeded = guard
            .as_mut()
            .is_some_and(|ctx| ctx.set_contents(owned.clone()).is_ok());
        if !succeeded {
            *guard = ClipboardContext::new().ok();
            if let Some(ctx) = guard.as_mut() {
                if ctx.set_contents(owned).is_err() {
                    *guard = None;
                }
            }
        }
    });
}

/// Read text from the persistent OS clipboard context, recreating the context
/// once on provider error (e.g. stale X11 connection).
pub fn read_clipboard_text() -> String {
    let from_os = std::panic::catch_unwind(|| {
        let mut guard = clipboard_holder()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if guard.is_none() {
            *guard = ClipboardContext::new().ok();
        }
        if let Some(ctx) = guard.as_mut() {
            match ctx.get_contents() {
                Ok(text) => return Some(text),
                Err(_) => {
                    *guard = ClipboardContext::new().ok();
                    if let Some(fresh) = guard.as_mut() {
                        match fresh.get_contents() {
                            Ok(text) => return Some(text),
                            Err(_) => {
                                *guard = None;
                            }
                        }
                    }
                }
            }
        }
        None
    })
    .ok()
    .flatten();

    from_os.unwrap_or_default()
}

/// Read clipboard text from the OS clipboard, falling back to `controller::anki`
/// and finally to the in-process fallback buffer (useful in headless environments).
pub fn read_copied_text_or_clipboard() -> String {
    let direct = read_clipboard_text();
    if !direct.is_empty() {
        return direct;
    }
    let anki_clip = crate::controller::anki::read_clipboard_text();
    if !anki_clip.is_empty() {
        return anki_clip;
    }
    fallback_clipboard()
        .lock()
        .ok()
        .map(|s| s.clone())
        .unwrap_or_default()
}

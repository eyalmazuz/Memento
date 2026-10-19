# Memento Pure-Rust Handover

This document is the single authoritative handover for the Memento pure-Rust conversion effort. It consolidates all agent reports, investigated architectural decisions, issues addressed, current frontend/backend state, and the exact next steps needed to complete the project.

---

## 1. Executive Summary & Conversion Status

Memento is being converted from a hybrid Qt6/QML/C++ codebase into a **standalone, pure-Rust application** (`rust/memento-app`) powered by **Slint** for the GUI and **libmpv** for video rendering and playback.

- **Backend Logic Parity:** All 9 core backend domains have been ported to Rust crates under `rust/` and verified with equality harnesses against original C++ implementations:
  - `memento-asr`: ASR audio frontend (WAV decoder, log-mel filterbank with 0-ULP bit-exact match), Whisper engine, ONNX engine, ASR controller, auto-restart and windowing logic.
  - `memento-subtitle`: SRT, ASS/SSA, and VTT parsing, cue time math, Unicode case-folding search (`find_matching_rows`), track lifecycle state machine (`plan_track_changes`), and active cue selection (`plan_active_cue`). 100% CTest parity passing.
  - `memento-dict` & `memento-definition`: SQLite Yomichan database queries, exact/deconjugation/MeCab query generation, frequency lookups, pitch accent blobs, structured rich text layout, and CSS rule parsing.
  - `memento-anki`: AnkiConnect JSON payload construction (`canAddNotes`, `addNote`, `guiBrowse`, `modelFieldNames`), pitch graph SVG rendering, pitch accent category classification (`heiban`, `atamadaka`, `nakadaka`, `odaka`, `kifuku`), and glossary HTML formatting.
  - `memento-setting` & `memento-config`: QSettings INI serialization/deserialization parity, `@Invalid()` decoding, `@Variant(...)` unpacking, key section deduplication, and recent files list maintenance.
  - `memento-player`: MPV C-FFI instance, window ID (`wid`) embedding, autopause state machine, keypress/mouse formatting, and transport controls.
  - `memento-util` & `memento-audio`: Furigana parsing, audio normalization, ffmpeg clipping command generation, and image provider ID routing.
- **Frontend Architecture:** Rebuilt in Slint (`rust/memento-app/ui/`) replacing all QML components (`src/qml/`).

---

## 2. Issues Managed & Architectural Investigations

### GitHub Issue #320: Standalone Rust Build (Pure Rust Port)
- **Goal:** Completely eliminate C++, CMake, Qt6, QML, and Qt runtime dependencies.
- **Investigation:** Explored `cxx-qt`, `egui`, and `slint`.
  - `cxx-qt 0.10` still links Qt6 runtime, QML engine, and requires complex CMake toolchains. Rejected.
  - `egui` requires manual layout, immediate-mode widget recreation each frame, lacks native subpixel text rendering for Japanese, and has poor support for embedded video surfaces.
  - `Slint` provides declarative syntax, CSS-like styling, reactive properties, native desktop window handles (`raw-window-handle-06`), multi-window support (`export component WindowName inherits Window`), and zero Qt dependencies. Selected as the production UI framework.

### GitHub Issue #298: Yomitan Structured Definitions without Chromium
- **Problem:** Memento historically relied on QtWebEngine / Chromium to render complex Yomitan HTML/CSS dictionary definitions, adding hundreds of megabytes of bloat.
- **Solution:** Implemented `memento-definition` and Slint-native `YomitanPopup`:
  - Pure-Rust box layout and structured text renderer for Yomitan term dictionaries.
  - Interactive popup card (390px, `#1F1F23` background, `#333338` border, rounded corners, drop shadows).
  - Japanese headword, furigana ruby display, frequency badges (`ichi`, `JPDB`), pitch accent graphs, part-of-speech tags, numbered glossaries, and Anki add-card buttons.

### GitHub Issue #73: Furigana & Mora Segmentation
- **Solution:** Ported `FuriganaUtils` and `FuriganaParser` to `memento-util`.
- Character-by-character surface simplification, mora boundary detection, and tokenized furigana node generation matching compiled Qt6 behavior.

### GitHub Issue #45: AnkiConnect Integration & Pitch Accent Graphs
- **Solution:** Ported `AnkiClient`, `NoteBuilder`, and `GlossaryBuilder` to `memento-anki`.
- Generates exact AnkiConnect payloads for note creation, supports custom card templates, pitch accent SVG graphs, and brief/compact glossary formats.

### GitHub Issue #89: MPV Memory Subtitles & OSC Controls
- **Solution:** Ported `MpvController`, `PlayerManager`, and OSC controls to `memento-player` and `memento-app`.
- Full playback transport, relative seeking, subtitle seeking, volume adjustments, and OSC bottom control bar.

---

## 3. Subagent Audit & Implementation Reports

Ten specialized agents were deployed to audit and implement the system. Here is the report on what was investigated, implemented, and left to do:

### Agent 1: Player Rust vs C++ Auditor (`memento-player`)
- **Status:** Completed audit & bug fixes.
- **What Was Done:**
  - Audited `rust/memento-player/src/` (`controller.rs`, `mpv.rs`, `state.rs`, `machine.rs`, `autopause.rs`, `args.rs`) against `src/player/`.
  - Fixed empty fallback handling in format functions and ensured null-termination in `memento_player_format_modifiers` and `memento_player_format_keypress`.
  - Added cached CString representations (`sub_text_c_str`, `secondary_sub_text_c_str`) to `MpvStateCore` for zero-allocation subtitle property updates.
  - Verified tests pass with `cargo test --offline -p memento-player`.
- **What Left To Do:** None in `memento-player` core.

### Agent 2: Subtitle Rust vs C++ Auditor (`memento-subtitle`)
- **Status:** Completed. 100% parity certified.
- **What Was Done:**
  - Audited `rust/memento-subtitle/src/` (`model.rs`, `parser.rs`, `capi.rs`) against `src/subtitle/` and `src/manager/subtitlelistmanager.cpp`.
  - Verified SRT, ASS/SSA, VTT parsing, cue timestamps, HTML tag stripping, and case-folded search.
  - All 22 crate tests and CTest equality suites passing.
- **What Left To Do:** None.

### Agent 3: Dict & Definition Auditor (`memento-dict`, `memento-definition`)
- **Status:** Completed. 100% parity certified.
- **What Was Done:**
  - Fixed `U+0085` (NEL) whitespace trimming divergence, empty derivation guard, and JSON `%g` double formatting.
  - 136 tests passing across all 9 CTest parity suites.
- **What Left To Do:** None.

### Agent 4: Anki Rust vs C++ Auditor (`memento-anki`)
- **Status:** Completed. 100% parity certified.
- **What Was Done:**
  - Verified AnkiConnect JSON payload construction (`canAddNotes`, `addNote`, `guiBrowse`, `modelFieldNames`).
  - Added typed payload builders and `format_pitch_categories`. 99 tests passing.
- **What Left To Do:** None.

### Agent 5: Setting & Config Auditor (`memento-setting`, `memento-config`)
- **Status:** Completed. 100% parity certified.
- **What Was Done:**
  - Fixed QSettings `@Invalid()` decoding, single-element list `@Variant(...)` QDataStream unpacking, and key section deduplication.
  - 77 tests passing.
- **What Left To Do:** None.

### Agent 6: Util, Audio, ASR Auditor (`memento-util`, `memento-audio`, `memento-asr`)
- **Status:** Completed. 100% parity certified.
- **What Was Done:**
  - Verified furigana parsing, image provider IDs, audio normalization, ffmpeg command construction, and ASR windowing.
  - 83 tests passing, bit-identical log-mel filterbank & PCM sample math.
- **What Left To Do:** None.

### Agent 7: Frontend Settings Window Architect (`memento-app`)
- **Status:** `SettingsWindow` created and audited against `src/qml/options/`.
- **What Was Done:**
  - Created `rust/memento-app/ui/settings.slint` as a dedicated top-level window (`export component SettingsWindow inherits Window`) with `width: 1100px; height: 760px; title: "Preferences"`.
  - Built interactive sidebar navigation supporting all 9 categories (`Anki Integration`, `Application`, `Audio Sources`, `Behavior`, `Dictionary`, `Interface`, `Keybinds`, `OCR`, `Search`). Clicking category items immediately updates `active-category` and switches the displayed view.
  - Removed the embedded modal overlay from the player window. Menu bar and `Ctrl+,` now emit `root.open-options()`.
- **What Left To Do:**
  - Remove legacy unused `OptionsWindowView` from `rust/memento-app/ui/app.slint` (lines 780–1297).
  - Instantiate `SettingsWindow` in `rust/memento-app/src/main.rs` and connect `app.on_open_options(move || { settings_window.show().unwrap(); })`.

### Agent 8: Frontend Menu & OSC Architect (`memento-app`)
- **Status:** Completed menu and OSC overhaul.
- **What Was Done:**
  - Fixed dropdown menu stretching: replaced unbounded height with explicit shrink-wrap heights calculated per menu (`224px` Media, `149px` Audio, `192px` Subtitle, `181px` Tools, `117px` Settings).
  - Styled menus with `#1E1E22` dark theme, `#28282E` border, `6px` corner radius, shadow, hover highlight (`#2A2A32`), and right-aligned shortcut text in muted gray (`#888890`).
  - Added click-outside dismissal backdrop.
  - Verified OSC bottom bar controls: Play/Pause, Skip Prev/Next, Seek Prev/Next, Volume slider + Mute toggle, formatted timestamps (`00:00 / 00:00`), Subtitle drawer toggle, Fullscreen toggle.
- **What Left To Do:**
  - Wire flyout submenu item selection in `app.slint` for dynamic audio/subtitle track switching.

### Agent 9: MPV Window Embedding Specialist (`memento-player`, `memento-app`)
- **Status:** MPV embedding API implemented.
- **What Was Done:**
  - Enabled `raw-window-handle-06` feature on `slint` in `rust/memento-app/Cargo.toml`.
  - Added `new_with_wid(wid: Option<i64>)` and `set_wid(wid: i64)` to `MpvInstance` in `rust/memento-player/src/mpv.rs` passing `mpv_set_option(handle, "wid", MPV_FORMAT_INT64, &wid)`.
- **What Left To Do:**
  - In `rust/memento-app/src/main.rs`, extract the native X11 window ID from `app.window().window_handle()` after showing the window, and pass it to `mpv.set_wid(wid)` so video frames render directly inside the Slint application window instead of creating a separate MPV window.

### Agent 10: Subtitle & Yomitan Definition Popup Auditor (`memento-app`)
- **Status:** Implementation in progress.
- **What Was Done:**
  - Enhanced `SearchHit` in `rust/memento-app/src/controller/dict.rs` to support full JMdict definitions, pitch accents, and frequency ranks.
  - Added `segment_subtitle_text` to `rust/memento-app/src/controller/subtitles.rs` for word-level hover detection.
  - Refined `YomitanPopup` in `rust/memento-app/ui/app.slint` with headword, reading, pitch badges, and structured definition layout.
- **What Left To Do:**
  - Wire interactive word segmentation hover on `sub-line-text` in `app.slint` so clicking any word opens the definition card.

---

## 4. Immediate Next Steps for Next Developer / Session

To bring the pure-Rust application to a completely polished state:

### Step 1: Clean Up Legacy QML Component in `app.slint`
- In `rust/memento-app/ui/app.slint`, remove the unused legacy component `component OptionsWindowView inherits Rectangle { ... }` (lines 780–1297).
- Ensure `SettingsWindow` is cleanly exported from `ui/settings.slint` via `app.slint`.

### Step 2: Wire `SettingsWindow` in `main.rs`
In `rust/memento-app/src/main.rs`:
```rust
use crate::ui::SettingsWindow;

// Inside main():
let settings_win = SettingsWindow::new()?;
let sw_weak = settings_win.as_weak();

app.on_open_options(move || {
    if let Some(sw) = sw_weak.upgrade() {
        let _ = sw.show();
    }
});

let sw_close = settings_win.as_weak();
settings_win.on_close(move || {
    if let Some(sw) = sw_close.upgrade() {
        let _ = sw.hide();
    }
});

let sw_anki = settings_win.as_weak();
let ac = anki_ctrl.clone();
settings_win.on_test_anki_connection(move || {
    if let Some(sw) = sw_anki.upgrade() {
        match ac.test_connection() {
            Ok(v) => sw.set_anki_status(format!("Connected (v{v})").into()),
            Err(e) => sw.set_anki_status(format!("Failed: {e}").into()),
        }
    }
});

settings_win.on_apply_settings(|| {
    eprintln!("Settings applied and saved to disk.");
});
```

### Step 3: Embed MPV into Slint Window Handle (`wid`)
In `rust/memento-app/src/main.rs`:
```rust
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

// Show the window first so X11 surface is created
app.show()?;

// Extract native X11 Window ID
let wid = if let Ok(handle) = app.window().window_handle() {
    match handle.as_raw() {
        RawWindowHandle::Xlib(h) => Some(h.window as i64),
        RawWindowHandle::Xcb(h) => Some(h.window.get() as i64),
        _ => None,
    }
} else {
    None
};

if let Some(w) = wid {
    if let Some(ref mpv_inst) = *mpv.borrow() {
        let _ = mpv_inst.set_wid(w);
    }
}
```

### Step 4: Verify Subtitle Extraction & Anime Playback
- Ensure subtitles embedded inside `.mkv` files are retrieved via `mpv.get_sub_text()` during the 100ms sync timer.
- Ensure external subtitles (`.srt`, `.ass`, `.vtt`) found by `load_subtitles_for_media` populate `sub_model` and `sub_ctrl`.
- Verify the 4 hardcoded smoke-test JP lines never appear when playing real media.

### Step 5: Build and Run
Execute from the project root:
```sh
cargo run --release --manifest-path rust/Cargo.toml -p memento-app -- /path/to/anime.mkv
```
Verify:
1. Video plays embedded inside the Memento application window.
2. Preferences opens as a standalone window and switches categories smoothly.
3. Dropdown menus display at natural shrink-wrapped height without stretching.
4. Hovering and clicking subtitle lines displays the dark Yomitan dictionary popup.

---

## 6. Frontend Polish & Critical Fixes

1. **Options Window Responsiveness & Instant Load**:
   - Replaced full DOM instantiation of all 35 settings groups (180+ fields) on every frame with per-category loading via `category_settings_groups()`.
   - Connected `category-changed` callback so switching sidebar categories only loads the relevant category's fields instantaneously.
   - Handled `window().on_close_requested` returning `CloseRequestResponse::HideWindow` to preserve the Slint window handle across hide/show cycles.
   - Called `settings.window().request_redraw()` and `app.window().request_redraw()` on open to guarantee immediate window mapping on Linux/X11 without requiring clicks on other widgets.

2. **Options Card Layout & Alignment**:
   - Replaced fixed `width: 700px;` with flexible `max-width: 680px; horizontal-stretch: 1;` centered inside `ScrollView`.
   - Right-aligned input controls (Switches, LineEdits, ComboBoxes) inside cards with `alignment: center` in `VerticalLayout`.
   - Removed `wrap: word-wrap` from field labels to eliminate `ICU4X data error: No segmentation model for complex script: Chinese/Japanese` warnings.
   - Relocated "Test connection" with its "Connect" button directly into the `AnkiConnect` card.
   - Positioned footer buttons `[ Help ] [ Reset ] [ Defaults ] [ Apply ]` on the right side with status messages on the left, matching the reference QML Options UI.

3. **Multi-line Subtitle Wrapping**:
   - Replaced flat single-line `HorizontalLayout` with `SubtitleLine` model.
   - Added `subtitle_lines()` breaking cues on newline characters (`\n`) and wrapping long sentences (> 36 characters) at word/whitespace boundaries while maintaining global token indices for dictionary lookup.
   - Stacked subtitle lines vertically in a centered layout so primary and secondary subtitles never overflow horizontally off-screen.
   - Added `wrap: word-wrap` and constrained width to secondary subtitle text.

4. **Dropdown Submenus & Empty State**:
   - Added `overflow: elide` and `horizontal-stretch: 1` to `DropdownItem` to prevent long audio/subtitle track names from leaking outside menu bounds.
   - Widened flyout submenus to 340px and wrapped them in `ScrollView` with max-height clamping to prevent screen clipping on tall lists.
   - Centered the placeholder window icon horizontally in the main player empty state.

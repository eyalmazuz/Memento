# Memento — Rust

Memento is a free and open-source video player for studying Japanese. This fork
uses Rust for the application, Slint for the interface, and libmpv for playback.
It builds on the original [Memento](https://github.com/ripose-jp/Memento).

## Branches

| Branch | Contents |
| --- | --- |
| [`master`](https://github.com/eyalmazuz/Memento/tree/master) | Active Rust application, promoted from the `rust` branch. |
| [`cpp`](https://github.com/eyalmazuz/Memento/tree/cpp) | Preserved C++/Qt `master`, including its source, build instructions, and history. |

The earlier C++ ASR work also remains on
[`feature/whisper-subtitle-provider-refactor`](https://github.com/eyalmazuz/Memento/tree/feature/whisper-subtitle-provider-refactor).
For the C++ application, use the [C++ README](https://github.com/eyalmazuz/Memento/blob/cpp/README.md).
Previously packaged C++ releases have separate installation instructions.

The Rust migration is still in progress; see the ASR integration limitation below.

## Features

* Grammar aware subtitle search
* Yomitan-style term and kanji lookup with multiple dictionary results
* Yomitan dictionaries, frequency badges, and pitch-accent graphs
* Anki card creation through [AnkiConnect](https://ankiweb.net/shared/info/2055492159)
* Selectable subtitle overlay, subtitle sidebar, and optional furigana
* Optional Whisper/ONNX speech recognition and Manga OCR
* mpv configuration, shaders, and scripts, subject to the ASR limitation below

## Build and run

Install current stable [Rust](https://rustup.rs/) and the system libraries below once. Then:

```bash
git clone https://github.com/eyalmazuz/Memento.git
cd Memento
cargo build
```

Run `cargo run`, or launch `target/debug/memento`. Add `--release` for a release build.
The default build includes playback, dictionaries, subtitles and Anki integration.

<details>
<summary>System packages (Linux)</summary>

Arch Linux:

```bash
sudo pacman -S --needed base-devel clang pkgconf mpv sqlite libzip json-c
```

Debian / Ubuntu:

```bash
sudo apt install build-essential libclang-dev pkg-config libmpv-dev libsqlite3-dev libzip-dev libjson-c-dev
```

Other distributions need the equivalent C/C++ toolchain, libclang, pkg-config,
and development libraries for libmpv, SQLite, libzip and json-c.
Cargo finds the installed libmpv automatically. No custom SDK or existing build
folder is needed. Linux is the currently tested platform.

</details>

### Optional build flags

ASR and OCR are disabled by default. Enable them with Cargo features:

| Feature | Command |
| --- | --- |
| Whisper ASR (CPU) | `cargo build --features asr` |
| Whisper ASR (CUDA) | `cargo build --features whisper-cuda` |
| Whisper ASR (Vulkan) | `cargo build --features whisper-vulkan` |
| ONNX ASR (CPU) | `cargo build --features asr-onnx` |
| OCR | `cargo build --features ocr` |
| Whisper ASR + OCR | `cargo build --features asr,ocr` |

Use the same flags with `cargo run`, for example `cargo run --features asr,ocr`.
Unavailable features are hidden in Settings.

Whisper builds need CMake and a C++ compiler. Cargo downloads and builds the pinned
Whisper sources automatically; ONNX builds download the matching official runtime.
Downloads are checksum-verified and cached in Cargo's build directory. CUDA requires
the CUDA toolkit; Vulkan requires the Vulkan SDK (including `glslc`). GPU support
is selected by the build flag; the **Use GPU** setting controls its use at runtime.

OCR runtime requirements and speech models are described below. Optional streaming
URL playback uses `yt-dlp`. Japanese fonts such as Noto Sans CJK JP are recommended.

See [development and playback diagnostics](docs/development.md) for tests,
performance measurements, and advanced build overrides.

## Dictionaries

* [JMdict](https://github.com/yomidevs/jmdict-yomitan/) (English)
  * [JMdict_english_with_examples.zip](https://github.com/yomidevs/jmdict-yomitan/releases/latest/download/JMdict_english_with_examples.zip)
    * This is the version of JMdict with example sentences from the [Tatoeba Project](http://tatoeba.org/home).
  * [JMdict_english.zip](https://github.com/yomidevs/jmdict-yomitan/releases/latest/download/JMdict_english.zip)
    * This is the same JMdict but without example sentences.
  * [JMdict_english_legacy.zip](https://github.com/yomidevs/jmdict-yomitan/releases/latest/download/JMdict_english_legacy.zip)
    * This is the same JMdict with legacy formatting instead of structured content.
  * [JMdict_english_without_proper_names.zip](https://github.com/yomidevs/jmdict-yomitan/releases/latest/download/JMdict_english_without_proper_names.zip)
    * This is `JMdict_english` but without several thousand entries from JMnedict.
  * [JMdict_english_legacy_without_proper_names.zip](https://github.com/yomidevs/jmdict-yomitan/releases/latest/download/JMdict_english_legacy_without_proper_names.zip)
* [JMdict](https://github.com/yomidevs/jmdict-yomitan/) (Other Languages)
  * [JMdict_dutch.zip](https://github.com/yomidevs/jmdict-yomitan/releases/latest/download/JMdict_dutch.zip)
  * [JMdict_french.zip](https://github.com/yomidevs/jmdict-yomitan/releases/latest/download/JMdict_french.zip)
  * [JMdict_german.zip](https://github.com/yomidevs/jmdict-yomitan/releases/latest/download/JMdict_german.zip)
  * [JMdict_hungarian.zip](https://github.com/yomidevs/jmdict-yomitan/releases/latest/download/JMdict_hungarian.zip)
  * [JMdict_russian.zip](https://github.com/yomidevs/jmdict-yomitan/releases/latest/download/JMdict_russian.zip)
  * [JMdict_slovenian.zip](https://github.com/yomidevs/jmdict-yomitan/releases/latest/download/JMdict_slovenian.zip)
  * [JMdict_spanish.zip](https://github.com/yomidevs/jmdict-yomitan/releases/latest/download/JMdict_spanish.zip)
  * [JMdict_swedish.zip](https://github.com/yomidevs/jmdict-yomitan/releases/latest/download/JMdict_swedish.zip)
* [Jitendex](https://jitendex.org/) (English)
  * [jitendex-yomitan.zip](https://github.com/stephenmk/stephenmk.github.io/releases/latest/download/jitendex-yomitan.zip)
    * Similar to `JMdict_english_with_examples` with additional formatting.
* [JMnedict](https://github.com/yomidevs/jmdict-yomitan/) (Japanese Names)
  * [JMnedict.zip](https://github.com/yomidevs/jmdict-yomitan/releases/latest/download/JMnedict.zip)
* [JPDB](https://github.com/Kuuuube/yomitan-dictionaries) (Frequency)
  * [JPDB v2.2 Kana](https://github.com/Kuuuube/yomitan-dictionaries/raw/main/dictionaries/JPDB_v2.2_Frequency_Kana_2024-10-13.zip) (Recommended)
  * [JPDB v2.2 Frequency Kana Display Only](https://github.com/Kuuuube/yomitan-dictionaries/raw/main/dictionaries/JPDB_v2.2_Frequency_Kana_Display_Only_2024-10-13.zip)
  * [JPDB v2.2](https://github.com/Kuuuube/yomitan-dictionaries/raw/main/dictionaries/JPDB_v2.2_Frequency_2024-10-13.zip)
* [BCCWJ SUW LUW Combined](https://github.com/Kuuuube/yomitan-dictionaries) (Frequency)
  * [BCCWJ_SUW_LUW_combined.zip](https://github.com/Kuuuube/yomitan-dictionaries/raw/main/dictionaries/BCCWJ_SUW_LUW_combined.zip)
* Kanjium (Pitch)
  * [kanjium_pitch_accents.zip](https://github.com/FooSoft/yomichan/raw/dictionaries/kanjium_pitch_accents.zip)
* [KANJIDIC](https://github.com/yomidevs/jmdict-yomitan/) (Kanji)
  * [KANJIDIC_english.zip](https://github.com/yomidevs/jmdict-yomitan/releases/latest/download/KANJIDIC_english.zip)
  * [KANJIDIC_french.zip](https://github.com/yomidevs/jmdict-yomitan/releases/latest/download/KANJIDIC_french.zip)
  * [KANJIDIC_portuguese.zip](https://github.com/yomidevs/jmdict-yomitan/releases/latest/download/KANJIDIC_portuguese.zip)
  * [KANJIDIC_spanish.zip](https://github.com/yomidevs/jmdict-yomitan/releases/latest/download/KANJIDIC_spanish.zip)

## Troubleshooting

### Can't Add Cards: Only the "Show in Anki" Button is Available

This means there is a mistake in your card template.
When this happens, AnkiConnect will report that all potential cards are not addable.
Double check your card template to see if the front of the card is missing something.

### Streaming URLs fail to open

Install or update `yt-dlp` and make it available to libmpv. See the C++ branch
for troubleshooting older packaged releases.

### OCR Support

Build with `--features ocr`. At runtime, OCR currently requires
[libmocr](https://github.com/ripose-jp/libmocr) and the Python `manga-ocr` package
in the Python environment used by libmocr. These are not required to compile
Memento. Installing `manga-ocr` alone does not install libmocr.

The OCR keybind (default: `Ctrl+L`) selects a video region for text recognition
and dictionary lookup.

### ASR (Speech-to-Text) Subtitles

Memento transcribes audio in a background worker and displays generated cues
in its Rust subtitle overlay and sidebar.
Speech recognition supports:

* **Whisper**: Local GGML models (e.g. `ggml-large-v3-turbo.bin` placed in `~/.config/memento/models/`).
* **ONNX Runtime**: HuggingFace ONNX-based speech models.

**Current integration limitation:** generated ASR cues are stored in Rust and
are not inserted into an mpv subtitle track. mpv subtitle properties, events,
and scripts therefore cannot see those cues. The current subtitle-seek and
auto-pause handlers still use mpv's subtitle state and are not correctly wired
to ASR cues. Loading the patched SDK alone does not fix this; the ASR controller
still needs to feed its in-memory subtitle API or another real mpv track.

## Configuration

Place mpv configuration files and scripts for this application in Memento's
configuration directory, separate from mpv's own configuration.

The Rust application uses these XDG directories, falling back to the paths
shown when the corresponding environment variable is unset:

| Purpose | Environment variable | Default directory |
| --- | --- | --- |
| Settings, mpv configuration, Anki profiles | `XDG_CONFIG_HOME` | `~/.config/memento` |
| Dictionaries and application data | `XDG_DATA_HOME` | `~/.local/share/memento` |
| Cache | `XDG_CACHE_HOME` | `~/.cache/memento` |

The Windows and macOS paths documented for the C++ application are available
in the [C++ README](https://github.com/eyalmazuz/Memento/blob/cpp/README.md).

## asbplayer

If you hate Memento, try [asbplayer](https://app.asbplayer.dev/).

## Acknowledgements

* UI inspired by Yomichan
  * https://github.com/FooSoft/yomichan
* Icons sourced from Google Material Symbols and Icons
  * https://fonts.google.com/icons
* OCR backend written by kha-white and contributors
  * https://github.com/kha-white/manga-ocr
* Most dictionary links taken from jmdict-yomitan and yomitan-dictionaries
  * https://github.com/yomidevs/jmdict-yomitan
  * https://github.com/Kuuuube/yomitan-dictionaries

# Subomi

**Subomi** (*蕾*) is a fast, open-source video player built for learning Japanese — written in Rust with a Slint interface and powered by `libmpv`.

Watch anime, dramas, or YouTube videos with instant hover dictionary lookups, pitch accent graphs, frequency rankings, automatic furigana, live speech-to-text subtitles, OCR, and one-click Anki card creation.

![Subomi Screenshot](example.png)

> Originally forked from [ripose-jp/Memento](https://github.com/ripose-jp/Memento) and inspired by [Yomitan](https://github.com/yomidevs/yomitan).

---

## Features

### Video, Themes & Subtitles
* **Cross-platform `libmpv` playback** — runs on Linux (X11/Wayland/Flatpak), Windows, and macOS with native file/folder dialogs, OS drag-and-drop, Finder/Dock file opening, and full support for your own `mpv.conf`, `input.conf`, shaders, scripts, playlists, hardware decoding, and streaming URLs via `yt-dlp`.
* **Light, Dark & System themes** — reactive light and dark palettes with automatic OS color-scheme detection (including GNOME/Flatpak via XDG Desktop Portal, Windows Registry, and macOS appearance).
* **Crisp vector-stroked & native ASS subtitles** — smooth per-glyph hover scanning (hover, modifier key, or middle-mouse), double-click subtitle copy, styled top-edge secondary subtitles, native `libass` rendering, and windowed/fullscreen cursor & OSC auto-hide.
* **Automatic furigana** — fast, lazy in-process MeCab tokenization supporting both **IPADic** and **UniDic** dictionaries (automatically unloaded when both furigana and MeCab lookup are disabled).
* **Interactive subtitle drawer** — Primary and Secondary tabs with in-list hover/middle-click dictionary scanning, instant Unicode-aware search with match navigation, `sub-delay`-aware timestamps & seeks, smooth in-place row updates, resizable split view, and multi-row selection for context/cloze mining.
* **Timeline navigation** — fast, aspect-preserving cached hover thumbnail previews and chapter markers.

### Yomitan-Style Dictionary Lookup
* **Rich formatted definitions** — renders Yomitan `structured-content` (tables, ruby text, collapsible `<details>`, local images, theme-tinted monochrome SVGs & rasters) with scoped dictionary `styles.css` (including `:root` custom CSS variables and Light/Dark theme rules), optional **compact glossaries**, **compact tags** deduplication, and click-to-show tag tooltips.
* **Accurate Japanese lookup** — comprehensive Yomitan-aligned Japanese deconjugation (Godan `-たら`/`-だら`, Kansai-ben, honorifics, slang, historical kana, Zuru verbs, irregulars), Japanese text preprocessors (emphatic sequence collapsing, kyūjitai → shinjitai standardization, WanaKana romaji → hiragana, radical/width normalization, CJK Extensions H & J), custom regex text replacement rules, optional MeCab compound/lemma matching, and an instant re-hover lookup cache.
* **Deep exploration** — rich interactive Dictionary Search panel with **prefix/wildcard search (`*` / `?`)**, clickable `?query=` cross-links with Back/Forward history, kanji entry footer on multi-character lookups, multi-definition cross-entry glossary selection (`Ctrl+C`), `かな` reading-as-expression toggle, and keyboard navigation (`Ctrl+Up`/`Ctrl+Down`/`Ctrl+D`).
* **Pitch accent & frequencies** — numeric downsteps and **string-based exact pitch patterns** (`"LHH"`, `"HLL"`, etc.), pitch graphs (with nasal and devoice markers), separated `rank-based` vs `occurrence-based` frequency modes, compact frequency badges (JPDB, BCCWJ, etc.), and category-colored tags.

### Anki Integration
* **One-click card presets & smooth setup** — built-in templates for **Lapis**, **Kiku**, **Senren**, **Animecards**, and **JPMN**, or custom field mappings via [AnkiConnect](https://ankiweb.net/shared/info/2055492159) (with API key support including batched `multi` actions, automatic deck/model discovery, marker/argument autocomplete, and an excluded-dictionary checklist).
* **Live card status & audio controls** — non-blocking duplicate detection (supporting `readingAsExpression`), **Browse in Anki** buttons, existing note tag pills, per-source audio availability indicators, and a right-click audio-source picker on the Add button.
* **Rich media, streaming & manual context** — `sub-delay`-aware and streaming-script-aware stereo `.aac` audio clips, **manual A/B audio context marking** (`Ctrl+B` / `Cmd+B`) for manual search & OCR lookups in subtitle-less video, lossy WebP/JPEG/PNG screenshots & video clips, multi-definition `{selection | clipboard | glossary}`, multi-subtitle cloze, non-fatal media fallback warnings, pitch graphs, frequencies, part-of-speech, and Yomitan HTML glossaries.

### Optional Speech Recognition (ASR) & OCR
* **Real-time ASR subtitles** — transcribe video audio on the fly using **Whisper** (CPU OpenMP/AVX2, Vulkan, or CUDA) or **ONNX Runtime** models (with low-peak-RAM arena allocation and 45 s idle model unloading). Generated cues feed natively into an in-memory `libmpv` subtitle track, so pause, seek, auto-pause, `sub-seek`, `sub-delay`, and audio track switching keep working seamlessly.
* **Manga OCR** — press `Ctrl+L` (customizable) to drag a box over hardcoded subtitles or on-screen Japanese text for instant recognition and lookup (right-click cancels; model automatically unloads after 5 minutes idle to reclaim RAM).

---

## Quick Start

Install stable [Rust](https://rustup.rs/) and the system libraries below, then build and run:

```bash
git clone https://github.com/eyalmazuz/Subomi.git
cd Subomi
cargo build --release
./target/release/subomi
```

The default build includes video playback, dictionaries, MeCab furigana/lookup, subtitles, and Anki integration. Pass a file path, URL, or standard `mpv` options directly on the command line:

```bash
./target/release/subomi "/path/to/video.mkv"
```

<details>
<summary><strong>Linux system dependencies</strong></summary>

**Arch Linux:**
```bash
sudo pacman -S --needed base-devel clang pkgconf mpv libass sqlite
```

**Debian / Ubuntu:**
```bash
sudo apt install build-essential libclang-dev pkg-config libmpv-dev libass-dev libsqlite3-dev
```

**Recommended runtime extras:**
* **Japanese fonts** (e.g., Noto Sans CJK JP) for clean subtitle and popup rendering
* **`mecab`** + an IPADic or UniDic dictionary for automatic furigana and MeCab lookup matching
* **`yt-dlp`** for opening streaming URLs

</details>

<details>
<summary><strong>macOS system dependencies</strong></summary>

Install via [Homebrew](https://brew.sh/):
```bash
brew install pkgconf mpv libass sqlite mecab mecab-ipadic yt-dlp
```
*(Homebrew paths `/opt/homebrew/bin` and `/usr/local/bin` are added automatically when launched from a `.app` bundle.)*

</details>

<details>
<summary><strong>Windows system dependencies</strong></summary>

* Rust stable MSVC toolchain (`x86_64-pc-windows-msvc`) and LLVM/Clang (for `bindgen`).
* `libmpv` (`mpv-2.dll` / import library), `libass`, and `sqlite3` development libraries on your build/runtime path.
* Optional runtime extras: `libmecab.dll` + IPADic/UniDic (placed next to `subomi.exe`, via `MECAB_DICDIR`, or standard MeCab install — paths with spaces are handled automatically via 8.3 short paths), `yt-dlp.exe`, and `libmocr.dll` (when built with `--features ocr`).

</details>

### Optional Build Features

ASR and OCR are opt-in via Cargo feature flags (unbuilt features are automatically hidden in Settings):

| Feature | Cargo flag | Notes |
| --- | --- | --- |
| **Whisper ASR (CPU)** | `--features asr` | OpenMP + AVX2/SIMD enabled by default |
| **Whisper ASR (Vulkan GPU)** | `--features whisper-vulkan` | Needs Vulkan headers & `glslc` |
| **Whisper ASR (CUDA GPU)** | `--features whisper-cuda` | Needs NVIDIA CUDA Toolkit |
| **ONNX ASR** | `--features asr-onnx` | Auto-discovers or fetches ONNX Runtime |
| **Manga OCR** | `--features ocr` | Loads `libmocr` at runtime |

**Build everything together** (example with Whisper Vulkan + ONNX + OCR):

```bash
cargo build --release --features ocr,asr,asr-onnx,whisper-vulkan
```

For smaller distribution binaries with fat LTO (slower link time than standard `--release`), use the `release-dist` profile:

```bash
cargo build --profile release-dist
```

> Whisper builds require `cmake` and a system toolchain (included in `base-devel` / `build-essential`). GPU backend support is chosen at build time; you can toggle **Use GPU** on or off anytime in Settings.
>
> For advanced build overrides, tests, and playback profiling, see [docs/development.md](docs/development.md).

---

## Recommended Dictionaries

Import one or multiple `.zip` dictionaries at once in **Settings → Dictionaries** (batch drag-and-drop or multi-file picker).

### Japanese Definitions & Names
* **[Jitendex](https://jitendex.org/)** *(Recommended English)* — [jitendex-yomitan.zip](https://github.com/stephenmk/stephenmk.github.io/releases/latest/download/jitendex-yomitan.zip)
* **[JMdict](https://github.com/yomidevs/jmdict-yomitan/)** *(English)*:
  * [JMdict_english_with_examples.zip](https://github.com/yomidevs/jmdict-yomitan/releases/latest/download/JMdict_english_with_examples.zip) *(includes Tatoeba example sentences)*
  * [JMdict_english.zip](https://github.com/yomidevs/jmdict-yomitan/releases/latest/download/JMdict_english.zip)
  * [JMdict_english_without_proper_names.zip](https://github.com/yomidevs/jmdict-yomitan/releases/latest/download/JMdict_english_without_proper_names.zip)
* **[JMnedict](https://github.com/yomidevs/jmdict-yomitan/)** *(Japanese Names)* — [JMnedict.zip](https://github.com/yomidevs/jmdict-yomitan/releases/latest/download/JMnedict.zip)

<details>
<summary>Other JMdict languages &amp; legacy formats</summary>

* [JMdict_dutch.zip](https://github.com/yomidevs/jmdict-yomitan/releases/latest/download/JMdict_dutch.zip)
* [JMdict_french.zip](https://github.com/yomidevs/jmdict-yomitan/releases/latest/download/JMdict_french.zip)
* [JMdict_german.zip](https://github.com/yomidevs/jmdict-yomitan/releases/latest/download/JMdict_german.zip)
* [JMdict_hungarian.zip](https://github.com/yomidevs/jmdict-yomitan/releases/latest/download/JMdict_hungarian.zip)
* [JMdict_russian.zip](https://github.com/yomidevs/jmdict-yomitan/releases/latest/download/JMdict_russian.zip)
* [JMdict_slovenian.zip](https://github.com/yomidevs/jmdict-yomitan/releases/latest/download/JMdict_slovenian.zip)
* [JMdict_spanish.zip](https://github.com/yomidevs/jmdict-yomitan/releases/latest/download/JMdict_spanish.zip)
* [JMdict_swedish.zip](https://github.com/yomidevs/jmdict-yomitan/releases/latest/download/JMdict_swedish.zip)
* [JMdict_english_legacy.zip](https://github.com/yomidevs/jmdict-yomitan/releases/latest/download/JMdict_english_legacy.zip)
* [JMdict_english_legacy_without_proper_names.zip](https://github.com/yomidevs/jmdict-yomitan/releases/latest/download/JMdict_english_legacy_without_proper_names.zip)

</details>

### Frequencies, Pitch Accent & Kanji
* **[JPDB Frequency](https://github.com/Kuuuube/yomitan-dictionaries)** *(Recommended)* — [JPDB v2.2 Kana](https://github.com/Kuuuube/yomitan-dictionaries/raw/main/dictionaries/JPDB_v2.2_Frequency_Kana_2024-10-13.zip) · [JPDB v2.2](https://github.com/Kuuuube/yomitan-dictionaries/raw/main/dictionaries/JPDB_v2.2_Frequency_2024-10-13.zip)
* **[BCCWJ Frequency](https://github.com/Kuuuube/yomitan-dictionaries)** — [BCCWJ_SUW_LUW_combined.zip](https://github.com/Kuuuube/yomitan-dictionaries/raw/main/dictionaries/BCCWJ_SUW_LUW_combined.zip)
* **Kanjium Pitch Accents** — [kanjium_pitch_accents.zip](https://github.com/FooSoft/yomichan/raw/dictionaries/kanjium_pitch_accents.zip)
* **[KANJIDIC](https://github.com/yomidevs/jmdict-yomitan/)** — [English](https://github.com/yomidevs/jmdict-yomitan/releases/latest/download/KANJIDIC_english.zip) · [French](https://github.com/yomidevs/jmdict-yomitan/releases/latest/download/KANJIDIC_french.zip) · [Portuguese](https://github.com/yomidevs/jmdict-yomitan/releases/latest/download/KANJIDIC_portuguese.zip) · [Spanish](https://github.com/yomidevs/jmdict-yomitan/releases/latest/download/KANJIDIC_spanish.zip)

---

## Feature Setup Notes

### Speech-to-Text (ASR)
* **Whisper**: Download models directly from **Settings → ASR**, or drop GGML models (such as `ggml-large-v3-turbo.bin` and optional Silero VAD `ggml-silero-v6.2.0.bin`) into the `models/` subdirectory of your Subomi config directory.
* **ONNX Runtime**: Supports local or downloaded ONNX speech models (e.g., `kitsune-transcribe-t0.6b-ja`) inside the same `models/` directory.

### Manga OCR
* Compile with `--features ocr`.
* At runtime, install [libmocr](https://github.com/ripose-jp/libmocr) and the Python [`manga-ocr`](https://github.com/kha-white/manga-ocr) package in your Python environment (on Windows, place `libmocr.dll` next to `subomi.exe`, in `%LOCALAPPDATA%\subomi`, or on `PATH`).
* Press **`Ctrl+L`** (default; **`Cmd+L`** on macOS) to drag a screen region on the video for instant OCR + dictionary lookup (right-click cancels).

### MeCab (Furigana & Compound Matching)
* Built in automatically — no Cargo feature flag required, and Subomi does not bundle a MeCab dictionary in the repo.
* Both **IPADic** (smallest/easiest via system packages) and **UniDic** *(recommended for noticeably more accurate modern readings and compound lookup)* are detected and supported automatically.
* **Install MeCab + a dictionary:**
  * **Debian / Ubuntu (IPADic):** `sudo apt install mecab libmecab2 mecab-ipadic-utf8`
  * **Arch Linux:** install `mecab` + `mecab-ipadic` (or `unidic`) from the AUR
  * **macOS (Homebrew):** `brew install mecab mecab-ipadic`
  * **UniDic via Python (any platform):** `pip install unidic && python -m unidic download`
* **Selecting your dictionary:** Subomi loads `libmecab` in-process (falling back to the `mecab` CLI) and automatically checks `MECAB_DICDIR`, bundled executable-adjacent `ipadic`/`unidic`/`dic` folders, `/etc/mecabrc` (`$MECABRC`), and standard Homebrew/Linux/Windows dictionary paths. If you install UniDic via Python or have multiple dictionaries installed, set `MECAB_DICDIR` or `dicdir = /path/to/unidic/dicdir` in `/etc/mecabrc` (run `python -c "import unidic; print(unidic.DICDIR)"` to find the path).
* **Enabling furigana:** Turn on **Show furigana over subtitles** in **Settings → Search** (or toggle it from the Subtitle menu / shortcut). MeCab loads lazily on first use and unloads automatically when both furigana and MeCab lookup are disabled.

---

## Configuration & Directories

Subomi keeps its `mpv.conf`, `input.conf`, `scripts/`, and `shaders/` in its own config folder, separate from your global `mpv` setup. On Linux, volatile state (`recent/files`, `dictionary/order`, window panel state) is split into the cache config file so your main `subomi.conf` stays clean for dotfile version control.

| Platform | Config (`subomi.conf`, `mpv.conf`, Anki, ASR models) | Data (dictionary DB & assets) | Cache (volatile state / temp) |
| --- | --- | --- | --- |
| **Linux** | `$XDG_CONFIG_HOME/subomi` (`~/.config/subomi`) | `$XDG_DATA_HOME/subomi` (`~/.local/share/subomi`) | `$XDG_CACHE_HOME/subomi` (`~/.cache/subomi`) |
| **macOS** | `~/Library/Application Support/subomi` *(falls back to `~/.config/subomi` if present)* | `~/Library/Application Support/subomi` | `~/Library/Caches/subomi` |
| **Windows** | `%LOCALAPPDATA%\subomi` *(falls back to `%APPDATA%\subomi` if present)* | `%LOCALAPPDATA%\subomi` | `%LOCALAPPDATA%\subomi\cache` |

---

## Troubleshooting

* **Can't add Anki cards (only "Browse in Anki" shows up):** Usually means the first field of your Anki note type is empty or misconfigured in your card template. Check your field mappings (or try a built-in preset under **Settings → Anki**).
* **Streaming URLs fail to open:** Make sure `yt-dlp` is installed, up to date, and on your `PATH`.
* **Looking for an alternative?** Check out [asbplayer](https://app.asbplayer.dev/) for browser-based sentence mining.

---

## Acknowledgements & License

* Originally forked from [ripose-jp/Memento](https://github.com/ripose-jp/Memento)
* Deconjugator foundations by Spacehamster
* Dictionary design & formatting inspired by [Yomitan](https://github.com/yomidevs/yomitan) and [Yomichan](https://github.com/FooSoft/yomichan)
* Icons from [Google Material Symbols](https://fonts.google.com/icons)
* OCR powered by [manga-ocr](https://github.com/kha-white/manga-ocr)
* Dictionary links from [jmdict-yomitan](https://github.com/yomidevs/jmdict-yomitan) and [yomitan-dictionaries](https://github.com/Kuuuube/yomitan-dictionaries)

Licensed under the [GNU General Public License v2.0](LICENSE).

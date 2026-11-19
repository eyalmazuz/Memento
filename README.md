# Memento

**Memento** is a fast, open-source video player built for learning Japanese — written in Rust with a Slint interface and powered by `libmpv`.

Watch anime, dramas, or YouTube videos with instant hover dictionary lookups, pitch accent graphs, frequency rankings, automatic furigana, live speech-to-text subtitles, OCR, and one-click Anki card creation.

![Memento Screenshot](example.png)

> Built upon the original [ripose-jp/Memento](https://github.com/ripose-jp/Memento) and inspired by [Yomitan](https://github.com/yomidevs/yomitan).

---

## Features

### Video & Subtitles
* **Full `libmpv` playback** — supports your own `mpv.conf`, `input.conf`, shaders, scripts, playlists, hardware decoding, drag-and-drop, and streaming URLs via `yt-dlp`.
* **Crisp vector-stroked subtitles** — smooth per-glyph hover scanning (hover, modifier key, or middle-mouse), top-edge secondary subtitles, and auto-hiding controls/cursor in fullscreen.
* **Automatic furigana** — fast in-process MeCab tokenization supporting both **IPADic** and **UniDic** dictionaries.
* **Interactive subtitle drawer** — Primary and Secondary tabs, instant search with match navigation, resizable split view, and multi-row selection for context/cloze mining.
* **Timeline navigation** — hover thumbnail previews and chapter markers.

### Yomitan-Style Dictionary Lookup
* **Rich formatted definitions** — renders Yomitan `structured-content` (tables, ruby text, collapsible `<details>`, local images, monochrome SVGs) with scoped dictionary `styles.css`.
* **Accurate Japanese lookup** — grammar-aware deconjugation, NFKC/kana normalization, conjugation hover tooltips, and optional MeCab compound/lemma matching.
* **Deep exploration** — clickable `?query=` cross-links with Back/Forward history, per-kanji lookup view, `かな` reading toggle, glossary text selection (`Ctrl+C`), and a dedicated Dictionary Search panel.
* **Pitch accent & frequencies** — pitch accent graphs (with nasal and devoice markers), frequency pills (JPDB, BCCWJ, etc.), and category-colored tags.

### Anki Integration
* **One-click card presets** — built-in templates for **Lapis**, **Kiku**, **Senren**, **Animecards**, and **JPMN**, or fully custom field mappings via [AnkiConnect](https://ankiweb.net/shared/info/2055492159).
* **Live card status** — non-blocking duplicate detection, **Browse in Anki** buttons, and existing note tag pills right inside the popup.
* **Rich media & markers** — stereo `.aac` audio clips, screenshots/video, multi-source word audio, multi-subtitle cloze, `{selection | clipboard | glossary}`, pitch graphs, frequencies, part-of-speech, and Yomitan HTML glossaries.

### Optional Speech Recognition (ASR) & OCR
* **Real-time ASR subtitles** — transcribe video audio on the fly using **Whisper** (CPU OpenMP/AVX2, Vulkan, or CUDA) or **ONNX Runtime** models. Generated cues feed natively into an in-memory `libmpv` subtitle track, so pause, seek, auto-pause, `sub-seek`, `sub-delay`, and audio track switching keep working seamlessly.
* **Manga OCR** — press `Ctrl+L` (customizable) to drag a box over hardcoded subtitles or on-screen Japanese text for instant recognition and lookup.

---

## Quick Start

Install stable [Rust](https://rustup.rs/) and the system libraries below, then build and run:

```bash
git clone https://github.com/eyalmazuz/Memento.git
cd Memento
cargo build --release
./target/release/memento
```

The default build includes video playback, dictionaries, MeCab furigana/lookup, subtitles, and Anki integration. Pass a file path, URL, or standard `mpv` options directly on the command line:

```bash
./target/release/memento "/path/to/video.mkv"
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

> Whisper builds require `cmake` and a system toolchain (included in `base-devel` / `build-essential`). GPU backend support is chosen at build time; you can toggle **Use GPU** on or off anytime in Settings.
>
> For advanced build overrides, tests, and playback profiling, see [docs/development.md](docs/development.md).

---

## Recommended Dictionaries

Import `.zip` dictionaries directly in **Settings → Dictionaries** (drag-and-drop or file picker).

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
* **Whisper**: Download models directly from **Settings → ASR**, or drop GGML models (such as `ggml-large-v3-turbo.bin` and optional Silero VAD `ggml-silero-v6.2.0.bin`) into `~/.config/memento/models/`.
* **ONNX Runtime**: Supports local or downloaded ONNX speech models (e.g., `kitsune-transcribe-t0.6b-ja`) inside `~/.config/memento/models/`.

### Manga OCR
* Compile with `--features ocr`.
* At runtime, install [libmocr](https://github.com/ripose-jp/libmocr) and the Python [`manga-ocr`](https://github.com/kha-white/manga-ocr) package in your Python environment.
* Press **`Ctrl+L`** (default) to select a screen region on the video for instant OCR + dictionary lookup.

### MeCab (Furigana & Compound Matching)
* Built in automatically — no Cargo feature flag required, and Memento does not bundle a MeCab dictionary in the repo.
* Both **IPADic** (smallest/easiest via distro packages) and **UniDic** *(recommended for noticeably more accurate modern readings and compound lookup)* are detected and supported automatically.
* **Install MeCab + a dictionary:**
  * **Debian / Ubuntu (IPADic):** `sudo apt install mecab libmecab2 mecab-ipadic-utf8`
  * **Arch Linux:** install `mecab` + `mecab-ipadic` (or `unidic`) from the AUR
  * **UniDic via Python (any distro):** `pip install unidic && python -m unidic download`
* **Selecting your dictionary:** Memento loads `libmecab` in-process (falling back to the `mecab` CLI) using whatever `dicdir` is configured in `/etc/mecabrc` (or the `$MECABRC` environment variable). If you install UniDic via Python or have multiple dictionaries installed, set `dicdir = /path/to/unidic/dicdir` in `/etc/mecabrc` (run `python -c "import unidic; print(unidic.DICDIR)"` to find the path).
* **Enabling furigana:** Turn on **Show furigana over subtitles** in **Settings → Search** (or toggle it from the Subtitle menu / shortcut). Without MeCab installed, subtitles still work normally without furigana.

---

## Configuration & Directories

Memento keeps its `mpv.conf`, `input.conf`, `scripts/`, and `shaders/` in its own config folder, separate from your global `mpv` setup.

| Purpose | Environment variable | Default path |
| --- | --- | --- |
| Settings, `mpv.conf`, Anki profiles, ASR models | `XDG_CONFIG_HOME` | `~/.config/memento` |
| Dictionary database & extracted dictionary assets | `XDG_DATA_HOME` | `~/.local/share/memento` |
| Cache | `XDG_CACHE_HOME` | `~/.cache/memento` |

---

## Troubleshooting

* **Can't add Anki cards (only "Browse in Anki" shows up):** Usually means the first field of your Anki note type is empty or misconfigured in your card template. Check your field mappings (or try a built-in preset under **Settings → Anki**).
* **Streaming URLs fail to open:** Make sure `yt-dlp` is installed, up to date, and on your `PATH`.
* **Looking for an alternative?** Check out [asbplayer](https://app.asbplayer.dev/) for browser-based sentence mining.

---

## Acknowledgements & License

* Original project by [ripose-jp/Memento](https://github.com/ripose-jp/Memento)
* Dictionary design & formatting inspired by [Yomitan](https://github.com/yomidevs/yomitan) and [Yomichan](https://github.com/FooSoft/yomichan)
* Icons from [Google Material Symbols](https://fonts.google.com/icons)
* OCR powered by [manga-ocr](https://github.com/kha-white/manga-ocr)
* Dictionary links from [jmdict-yomitan](https://github.com/yomidevs/jmdict-yomitan) and [yomitan-dictionaries](https://github.com/Kuuuube/yomitan-dictionaries)

Licensed under the [GNU General Public License v2.0](LICENSE).

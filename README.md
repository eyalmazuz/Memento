<div align="center">

<img src="res/subomi.svg" width="116" height="116" alt="Subomi logo" />

# Subomi · 蕾

**A fast, native Japanese immersion video player — built in Rust & Slint on `libmpv`.**

*Watch · Hover · Mine — instant Yomitan lookups, pitch accent graphs, automatic furigana, real-time speech-to-text subtitles, Manga OCR, and one-click Anki cards without leaving the player.*

<p>
  <img src="https://img.shields.io/badge/Rust-2021-9333EA?style=flat-square&logo=rust&logoColor=white" alt="Rust" />
  <img src="https://img.shields.io/badge/UI-Slint%20%2B%20libmpv-7E22CE?style=flat-square" alt="Slint + libmpv" />
  <img src="https://img.shields.io/badge/Platforms-Linux%20%C2%B7%20macOS%20%C2%B7%20Windows-32846B?style=flat-square" alt="Platforms" />
  <img src="https://img.shields.io/badge/Ecosystem-Yomitan%20%C2%B7%20AnkiConnect-D97706?style=flat-square" alt="Yomitan and AnkiConnect" />
  <a href="LICENSE"><img src="https://img.shields.io/badge/License-GPL--2.0-22C55E?style=flat-square" alt="GPL-2.0" /></a>
</p>

<p>
  <a href="#highlights">Highlights</a> •
  <a href="#quick-start">Quick Start</a> •
  <a href="#optional-features">ASR &amp; OCR</a> •
  <a href="#dictionaries">Dictionaries</a> •
  <a href="#directories">Directories</a>
</p>

<br />

<img src="example.png" alt="Subomi Screenshot" width="100%" />

<sub><em>Per-glyph subtitle scanning with Yomitan structured glossaries, pitch accent graphs, JPDB frequencies, and one-click Anki mining.</em></sub>

</div>

---

<a id="highlights"></a>
## ✨ Highlights

<table>
<tr>
<td width="50%" valign="top">

### 🎬 Video & Subtitles
* **Native `libmpv` + `libass`** on Linux (X11/Wayland/Flatpak), macOS, and Windows with full `mpv.conf`, `input.conf`, shaders, scripts, playlists, hwdec, and `yt-dlp` streaming.
* **Per-glyph hover scanning** over crisp vector-stroked or ASS subtitles, plus top-edge secondary subtitles and double-click copy.
* **Interactive subtitle drawer** with Primary/Secondary tabs, instant search, resizable split view, and multi-row selection for context mining.
* **Timeline hover thumbnails**, chapter markers, and reactive **Light / Dark / System** themes.

</td>
<td width="50%" valign="top">

### 📖 Yomitan-Style Dictionary
* **Rich `structured-content`** rendering with scoped dictionary `styles.css` (`:root` variables, Light/Dark rules), compact glossaries/tags, images, and tag tooltips.
* **Full Japanese deconjugation** & preprocessors (Kansai-ben, honorifics, slang, kyūjitai → shinjitai, WanaKana romaji, custom regex rules, optional MeCab matching).
* **Pitch accent & frequencies** — downsteps, exact `"LHH"`/`"HLL"` patterns, pitch graphs (nasal/devoice), separate rank & occurrence stats.
* **Interactive search panel** with `*`/`?` wildcards, `?query=` cross-links + history, kanji footer, and multi-entry selection.

</td>
</tr>
<tr>
<td width="50%" valign="top">

### ⚡ One-Click Anki Mining
* **Built-in card presets** for **Lapis**, **Kiku**, **Senren**, **Animecards**, and **JPMN**, or custom [AnkiConnect](https://ankiweb.net/shared/info/2055492159) templates with API key & marker autocomplete.
* **Live card status** — non-blocking duplicate badges, *Browse in Anki* links, existing tag pills, and right-click audio source picker.
* **Rich media capture** — `sub-delay`-aware `.aac` audio & video clips, WebP/JPEG/PNG screenshots, multi-subtitle cloze, and **manual A/B audio context** for subtitle-less videos.

</td>
<td width="50%" valign="top">

### 🎙️ Live ASR, OCR & Furigana
* **Real-time speech-to-text** via **Whisper** (CPU / Vulkan / CUDA) or **ONNX Runtime**, fed directly into an in-memory `libmpv` subtitle track (auto-unloads after 45 s idle).
* **Built-in Manga OCR** — drag over hardcoded subs or on-screen text for instant lookup. Runs `manga-ocr` in an isolated Python worker (no `libmocr`, auto-unloads after 5 min idle to free RAM/VRAM).
* **Automatic furigana** via lazy in-process **MeCab** (IPADic & UniDic).

</td>
</tr>
</table>

<p align="center">
  <em><strong>Watch</strong> local files or streams &nbsp;→&nbsp; <strong>Hover</strong> any glyph or drag-OCR &nbsp;→&nbsp; <strong>Mine</strong> audio, screenshot &amp; pitch straight to Anki.</em>
</p>

---

<a id="quick-start"></a>
## 🚀 Quick Start

Install stable [Rust](https://rustup.rs/) and the system libraries for your OS, then build and run:

```bash
git clone https://github.com/eyalmazuz/Subomi.git
cd Subomi
cargo build --release
./target/release/subomi "/path/to/video.mkv"   # files, URLs, or mpv flags
```

> [!TIP]
> Default builds already include video playback, Yomitan dictionaries, MeCab furigana/lookup, subtitle drawers, and full AnkiConnect mining.

<details>
<summary><strong>Linux dependencies</strong> (Arch / Debian / Ubuntu)</summary>

```bash
# Arch Linux
sudo pacman -S --needed base-devel clang pkgconf mpv libass sqlite

# Debian / Ubuntu
sudo apt install build-essential libclang-dev pkg-config libmpv-dev libass-dev libsqlite3-dev
```

*Recommended runtime extras:* Japanese fonts (`noto-fonts-cjk`), `mecab` + IPADic/UniDic (furigana), and `yt-dlp` (streaming URLs).

</details>

<details>
<summary><strong>macOS dependencies</strong> (Homebrew)</summary>

```bash
brew install pkgconf mpv libass sqlite mecab mecab-ipadic yt-dlp
```

*Homebrew paths (`/opt/homebrew/bin`, `/usr/local/bin`) are added automatically when launched from a `.app` bundle.*

</details>

<details>
<summary><strong>Windows dependencies</strong> (MSVC)</summary>

* Stable Rust MSVC toolchain (`x86_64-pc-windows-msvc`) + LLVM/Clang (for `bindgen`).
* `libmpv` (`mpv-2.dll` / import library), `libass`, and `sqlite3` dev libraries on your build/runtime path.
* *Optional runtime extras:* `libmecab.dll` + IPADic/UniDic (next to `subomi.exe`, via `MECAB_DICDIR`, or standard install), `yt-dlp.exe`, and Python 3 + `pip install manga-ocr` (for `--features ocr`).

</details>

---

<a id="optional-features"></a>
## 🧩 Optional Features

ASR and OCR are opt-in Cargo features (unbuilt features are cleanly hidden in Settings):

| Feature | Cargo Flag | Requirements |
| --- | --- | --- |
| **Whisper ASR (CPU)** | `--features asr` | `cmake` + C/C++ toolchain (OpenMP + SIMD enabled) |
| **Whisper ASR (Vulkan)** | `--features whisper-vulkan` | Vulkan headers & `glslc` |
| **Whisper ASR (CUDA)** | `--features whisper-cuda` | NVIDIA CUDA Toolkit |
| **ONNX ASR** | `--features asr-onnx` | Auto-discovers or fetches ONNX Runtime |
| **Manga OCR** | `--features ocr` | Pure Rust — just `pip install manga-ocr` at runtime |

```bash
# Full-featured build (example: OCR + Whisper Vulkan + ONNX ASR)
cargo build --release --features ocr,asr,asr-onnx,whisper-vulkan

# Smaller distribution binary with fat LTO
cargo build --profile release-dist --features ocr,asr,asr-onnx,whisper-vulkan
```

> [!NOTE]
> See [docs/development.md](docs/development.md) for custom native library overrides (`ORT_LIB_LOCATION`, `MPV_SDK`), running tests, and playback diagnostics.

<details>
<summary><strong>Setup notes: ASR models, Manga OCR, &amp; MeCab furigana</strong></summary>

* **Whisper / ONNX ASR:** Download models directly in **Settings → ASR**, or place GGML models (e.g., `ggml-large-v3-turbo.bin`, `ggml-silero-v6.2.0.bin`) or ONNX model folders (`kitsune-transcribe-t0.6b-ja`) inside `<config-dir>/models/`. Toggle GPU anytime in Settings.
* **Manga OCR (`--features ocr`):** Run `pip install manga-ocr` in your system Python or a virtualenv. Subomi automatically discovers `SUBOMI_OCR_PYTHON`, `VIRTUAL_ENV`, `CONDA_PREFIX`, `<data-dir>/ocr-venv`, and system `python3`/`python` — no `libmocr` needed.
* **MeCab furigana:** Works out of the box once `mecab` + **IPADic** or **UniDic** *(recommended)* is installed:
  * **Debian / Ubuntu:** `sudo apt install mecab libmecab2 mecab-ipadic-utf8`
  * **Arch Linux:** `mecab` + `mecab-ipadic` or `unidic` from AUR
  * **macOS:** `brew install mecab mecab-ipadic`
  * **UniDic (any OS):** `pip install unidic && python -m unidic download` *(point `MECAB_DICDIR` or `/etc/mecabrc` at `python -c "import unidic; print(unidic.DICDIR)"` if needed)*

</details>

---

<a id="dictionaries"></a>
## 📚 Dictionaries

Drag-and-drop or batch-import Yomitan `.zip` archives in **Settings → Dictionaries**:

| Category | Dictionary | Download |
| --- | --- | --- |
| **Definitions** | **[Jitendex](https://jitendex.org/)** *(Recommended)* | [jitendex-yomitan.zip](https://github.com/stephenmk/stephenmk.github.io/releases/latest/download/jitendex-yomitan.zip) |
| **Definitions** | **[JMdict](https://github.com/yomidevs/jmdict-yomitan/)** (English) | [With examples](https://github.com/yomidevs/jmdict-yomitan/releases/latest/download/JMdict_english_with_examples.zip) · [Standard](https://github.com/yomidevs/jmdict-yomitan/releases/latest/download/JMdict_english.zip) |
| **Names** | **[JMnedict](https://github.com/yomidevs/jmdict-yomitan/)** | [JMnedict.zip](https://github.com/yomidevs/jmdict-yomitan/releases/latest/download/JMnedict.zip) |
| **Frequencies** | **[JPDB](https://github.com/Kuuuube/yomitan-dictionaries)** *(Recommended)* | [JPDB v2.2](https://github.com/Kuuuube/yomitan-dictionaries/raw/main/dictionaries/JPDB_v2.2_Frequency_2024-10-13.zip) · [Kana](https://github.com/Kuuuube/yomitan-dictionaries/raw/main/dictionaries/JPDB_v2.2_Frequency_Kana_2024-10-13.zip) |
| **Frequencies** | **[BCCWJ](https://github.com/Kuuuube/yomitan-dictionaries)** | [BCCWJ_SUW_LUW_combined.zip](https://github.com/Kuuuube/yomitan-dictionaries/raw/main/dictionaries/BCCWJ_SUW_LUW_combined.zip) |
| **Pitch Accent** | **Kanjium** | [kanjium_pitch_accents.zip](https://github.com/FooSoft/yomichan/raw/dictionaries/kanjium_pitch_accents.zip) |
| **Kanji** | **[KANJIDIC](https://github.com/yomidevs/jmdict-yomitan/)** | [English](https://github.com/yomidevs/jmdict-yomitan/releases/latest/download/KANJIDIC_english.zip) · [FR](https://github.com/yomidevs/jmdict-yomitan/releases/latest/download/KANJIDIC_french.zip) · [PT](https://github.com/yomidevs/jmdict-yomitan/releases/latest/download/KANJIDIC_portuguese.zip) · [ES](https://github.com/yomidevs/jmdict-yomitan/releases/latest/download/KANJIDIC_spanish.zip) |

<details>
<summary>Other JMdict languages &amp; legacy packs</summary>

* [Dutch](https://github.com/yomidevs/jmdict-yomitan/releases/latest/download/JMdict_dutch.zip) · [French](https://github.com/yomidevs/jmdict-yomitan/releases/latest/download/JMdict_french.zip) · [German](https://github.com/yomidevs/jmdict-yomitan/releases/latest/download/JMdict_german.zip) · [Hungarian](https://github.com/yomidevs/jmdict-yomitan/releases/latest/download/JMdict_hungarian.zip) · [Russian](https://github.com/yomidevs/jmdict-yomitan/releases/latest/download/JMdict_russian.zip) · [Slovenian](https://github.com/yomidevs/jmdict-yomitan/releases/latest/download/JMdict_slovenian.zip) · [Spanish](https://github.com/yomidevs/jmdict-yomitan/releases/latest/download/JMdict_spanish.zip) · [Swedish](https://github.com/yomidevs/jmdict-yomitan/releases/latest/download/JMdict_swedish.zip)
* [English (no proper names)](https://github.com/yomidevs/jmdict-yomitan/releases/latest/download/JMdict_english_without_proper_names.zip) · [Legacy English](https://github.com/yomidevs/jmdict-yomitan/releases/latest/download/JMdict_english_legacy.zip)

</details>

---

<a id="directories"></a>
## 🗂️ Directories

Subomi keeps its own `mpv.conf`, `input.conf`, `scripts/`, and `shaders/` isolated from your global `mpv` config. On Linux, volatile state (`recent/files`, window/panel state) is split into the cache config so `subomi.conf` stays clean for dotfiles.

| Platform | Config (`subomi.conf`, `mpv.conf`, models) | Data (dictionaries & assets) | Cache |
| --- | --- | --- | --- |
| **Linux** | `~/.config/subomi` | `~/.local/share/subomi` | `~/.cache/subomi` |
| **macOS** | `~/Library/Application Support/subomi` | `~/Library/Application Support/subomi` | `~/Library/Caches/subomi` |
| **Windows** | `%LOCALAPPDATA%\subomi` | `%LOCALAPPDATA%\subomi` | `%LOCALAPPDATA%\subomi\cache` |

<details>
<summary><strong>Troubleshooting</strong></summary>

* **Only "Browse in Anki" appears instead of Add:** Your note type's first field is empty or unmapped — verify mappings or apply a preset under **Settings → Anki**.
* **Streaming URLs fail to open:** Ensure an up-to-date `yt-dlp` binary is on your `PATH`.
* **Looking for a browser player instead?** Try [asbplayer](https://app.asbplayer.dev/).

</details>

---

<div align="center">

<img src="res/subomi.svg" width="28" height="28" alt="Subomi" />

<sub>
Originally forked from <a href="https://github.com/ripose-jp/Memento">ripose-jp/Memento</a> · Inspired by <a href="https://github.com/yomidevs/yomitan">Yomitan</a> &amp; <a href="https://github.com/FooSoft/yomichan">Yomichan</a> · Deconjugator foundations by Spacehamster · Icons from <a href="https://fonts.google.com/icons">Material Symbols</a> · OCR powered by <a href="https://github.com/kha-white/manga-ocr">manga-ocr</a>
<br />
Licensed under the <a href="LICENSE">GNU General Public License v2.0</a>
</sub>

</div>

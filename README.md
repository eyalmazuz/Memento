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
* Whisper and ONNX speech recognition
* mpv configuration, shaders, and scripts, subject to the ASR limitation below

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

## Dependencies

The current development and playback checks run on Linux.

* **Rust and Cargo**: use a current stable toolchain. Slint 1.16.1 requires at
  least Rust 1.88; the locked dependency set may require a newer compiler.
* **libmpv SDK**: matching development headers and library. The current build
  has been exercised with the patched SDK; full stock-libmpv compatibility
  has not been verified.
* **Native build tools**: a C/C++ toolchain, Clang/libclang for bindgen, and
  CMake when building the native speech backends.
* **Dictionary libraries**: SQLite, libzip, and json-c development libraries.
* **Speech libraries**: whisper.cpp/ggml and ONNX Runtime. The current desktop
  binary links both backends; see the native dependency setup below.
* **yt-dlp** (optional, for streaming URL playback)
* **manga-ocr** (optional, for Manga OCR text extraction: `pip install manga-ocr`)

For the best experience, install [Noto Sans CJK JP](https://github.com/googlefonts/noto-cjk/raw/main/Sans/Variable/TTF/NotoSansCJKjp-VF.ttf)
and the [Kanji Stroke Order](https://drive.google.com/uc?export=download&id=1oyQoTB531tbhlYaOW7ugvutXZ7HSlJfW) fonts.

## Building and Running

### Native dependency setup

The Rust application uses Cargo, while libmpv and the speech engines remain
native dependencies. Cargo currently reuses prepared native libraries; a fresh
clone does not download or build all of them automatically.

Set `MPV_SDK` to the SDK prefix containing `include/mpv/client.h`, the rendering
headers, and `lib/libmpv.so` (or `lib64/libmpv.so`):

```bash
export MPV_SDK=/path/to/mpv-sdk
```

Existing `build*/CMakeCache.txt` files can also supply the SDK location.
The top-level `build.rs` looks for prepared speech libraries in
`build-whisper-cuda`, `build-rust-master`, and `build`. Set
`MEMENTO_WHISPER_BUILD_DIR` to add another build directory. Each directory uses
the existing native dependency layout:

```text
_deps/whispercpp-build/src/libwhisper.a
_deps/whispercpp-build/ggml/src/libggml*.a
_deps/onnxruntime_prebuilt-src/lib/libonnxruntime.so
```

The checked-in Whisper bindings target whisper.cpp v1.9.4, and the ONNX backend
targets ONNX Runtime API 1.22.0. Keep the headers and native libraries compatible.

### Build and run

Once the native dependencies are available, build the application:

```bash
cargo build --release
```

The Whisper **Use GPU** setting selects a GPU only when the linked whisper.cpp
build contains a CUDA or Vulkan backend and the graphics driver exposes a
device. A CPU-only build continues on the CPU and shows a warning when GPU is
requested. For NVIDIA CUDA on Linux, check that `nvidia-smi` works, then build
the backend and relink Memento:

```bash
cmake -S build-rust-master/_deps/whispercpp-src \
  -B build-whisper-cuda/_deps/whispercpp-build \
  -DGGML_CUDA=ON \
  -DWHISPER_BUILD_EXAMPLES=OFF -DBUILD_SHARED_LIBS=OFF
cmake --build build-whisper-cuda/_deps/whispercpp-build --target whisper -j"$(nproc)"
cargo build --release
```

Set `CUDA_HOME` when the toolkit is outside `/opt/cuda`. Vulkan builds use
`-DGGML_VULKAN=ON` and target `whisper` instead.
If CMake cannot see a GPU during configuration, add
`-DCMAKE_CUDA_ARCHITECTURES=all-major` for a portable CUDA build. To use
ccache, set `CMAKE_C_COMPILER_LAUNCHER`, `CMAKE_CXX_COMPILER_LAUNCHER`, and
`CMAKE_CUDA_COMPILER_LAUNCHER` to `ccache`; point `CCACHE_DIR` at a writable
directory when building in a restricted environment.

Run the application:
```bash
cargo run --release
```

Plain `cargo run` also optimizes Slint's UI core, renderer, text layout and
font parsing/rasterization libraries, while retaining debug symbols and assertions.
These settings do not select a CPU/GPU architecture or rebuild the native Whisper
backend. System fallback font metadata is cached before playback starts, avoiding
Fontconfig scans when dictionary entries introduce unfamiliar symbols.

For playback diagnostics, run `MEMENTO_VIDEO_STATS=1 cargo run`. Every five
seconds during rendering, `[video]` reports frame drops, the active hardware decoder,
pause state, frame delivery delay, mpv render time and total UI drawing time.
`draw-popup-rows` counts the dictionary entries open during the slowest draw;
unchanged idle intervals are omitted. Slow frames and
changes in drop counts are also reported without this environment variable.
`wake-max` measures delivery to the UI callback; `sync-max` measures the playback
UI synchronization callback, and `sync-gap` measures time between its ticks
(normally about 100 ms). `pending` shows the age of a frame still awaiting drawing.
These distinguish event-loop/layout stalls from time spent inside the renderer.
`present-max` measures the delay from submitted drawing to the next event-loop
callback, including buffer presentation.
On Linux hybrid GPUs, an explicit `hwdec=nvdec` selects NVIDIA PRIME rendering
before creating the window when the NVIDIA driver is accessible. This keeps
decoding and rendering on the same GPU. Explicit graphics environment settings
(such as `DRI_PRIME`, `__NV_PRIME_RENDER_OFFLOAD`, and `SLINT_BACKEND`) take
precedence. When the window uses Intel/AMD, direct `nvdec` falls back to
`nvdec-copy` for compatibility. Other decoder choices retain the system's default
rendering device. When a subtitle is present at startup, playback waits for its
furigana to be drawn before starting the playback clock.

Run the test suite:
```bash
cargo test
```

To include the popup geometry checks, run:

```bash
SLINT_EMIT_DEBUG_INFO=1 cargo test --test dictionary_popup_layout_test
```

Rust build and test commands currently run locally through Cargo. This branch
has no GitHub Actions workflows or legacy Windows/Gentoo packaging. The original
C++ build and packaging files remain in the `cpp` branch.

The optional Linux playback timing test renders the actual Slint window through
surfaceless EGL, including the video image, controls and changing furigana subtitles:

```bash
MEMENTO_VIDEO_TEST_MEDIA=/path/to/video.mkv MEMENTO_VIDEO_TEST_SECONDS=30 \
  cargo test --test video_render_timing_test -- --ignored --nocapture
```

It measures video and UI rendering together, but does not test a desktop
compositor or physical display. Without a media path it generates a 1080p test
pattern through libavfilter.
Set `MEMENTO_VIDEO_TEST_POPUP=1` to also open a dictionary popup during playback.
First-use shader compilation by the software GL driver can affect this test;
`MESA_SHADER_CACHE_DIR` can point to a writable directory when the default
Mesa cache is inaccessible.

For the interaction stress run, use a finite video at least 30 seconds long and
an existing dictionary database (opened read-only):

```bash
MEMENTO_VIDEO_TEST_MEDIA=/path/to/video.mkv MEMENTO_VIDEO_TEST_SECONDS=30 \
MEMENTO_VIDEO_TEST_INTERACTIONS=1 \
MEMENTO_VIDEO_TEST_DICTIONARY=/path/to/dictionaries.sqlite \
  cargo test --test video_render_timing_test -- --ignored --nocapture
```

This run toggles furigana and subtitle visibility, performs five term searches,
scrolls and collapses definitions, changes a definition's selection, pauses and
resumes, resizes the window, and seeks forward/backward to seeded random positions.
Drop counts are accumulated across seeks, so resetting mpv's counter cannot
hide an earlier stall. It exercises the shared UI/controller code through the
headless window; desktop fullscreen, input delivery and presentation still need
a desktop session.

To check a cold subtitle start without searches or furigana toggles, use a video
with embedded Japanese subtitles and `MEMENTO_VIDEO_TEST_STARTUP_FURIGANA=1`
instead of `MEMENTO_VIDEO_TEST_INTERACTIONS`. This uses mpv's actual subtitle
notifications and the desktop's subtitle model publication functions, leaving
furigana enabled throughout the run. `MEMENTO_VIDEO_TEST_START=335` starts at a
chosen timestamp, which also checks loading a video in the middle of a subtitle.

### OCR Support

To use OCR in Memento, install `manga-ocr` with Python:
```bash
pip install manga-ocr
```
Once installed, OCR can be triggered with the OCR keybind (default: `Ctrl+L`) to select screen regions and look up definitions directly in Yomitan dictionaries.

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

# Development and playback diagnostics

## Build overrides

Normal builds use `cargo build`; see the [README](../README.md#build-and-run).
These overrides are only for custom native installations:

* `MPV_SDK`: prefix with `include/mpv` and `lib` (or `lib64`). Alternatively,
  set `MPV_INCLUDE_DIR` and `MPV_LIB_DIR` separately.
* `ORT_LIB_LOCATION`: an ONNX Runtime 1.22.0 installation or its library directory,
  instead of downloading the official CPU runtime with `--features asr-onnx`.
* `MEMENTO_WHISPER_BUILD_DIR`: a prepared build containing
  `_deps/whispercpp-build`, instead of Cargo building Whisper 1.9.4.
  Select matching `whisper-cuda`/`whisper-vulkan` features for GPU libraries.
* `WHISPER_INCLUDE_DIR` and `GGML_INCLUDE_DIR`: matching headers when using a
  custom Whisper build. Otherwise the checked-in 1.9.4 bindings are used.
* `CUDA_HOME` / `CUDA_PATH`: CUDA toolkit location when it is outside the usual
  installation paths. `CMAKE_CUDA_ARCHITECTURES` optionally selects CUDA targets.

Cargo uses the available CPUs by default (`-j N` overrides this). Compiler caching
is left enabled when configured in the environment. Whisper follows Rust's target
CPU features instead of requiring a fixed CPU architecture.

## Playback diagnostics

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

# libmpv Memory-Subtitle Regression

This black-box test checks the in-memory subtitle command API against an
installed libmpv SDK. It covers track creation/removal, cue IDs and mutations,
`sub-lines` and `sub-text`, observed property events, file-change cleanup, and
the raw-cue timing contract under `sub-speed=0.5` and `sub-delay=0.25`. The
timing case seeks to 1.0s and verifies a cue at 1.0s is active while
`sub-start` and `sub-end` remain raw cue times. It drives deterministic
segments through a fake producer instead of loading Whisper weights, and
synthesizes its video fixture locally.

Run it on Linux with a C compiler, FFmpeg, and the SDK prefix set explicitly:

```sh
MPV_SDK=/path/to/libmpv-sdk sh tests/mpv-memory-subtitles/run.sh
```

The full ASR-enabled CMake build also registers the fake-ASR integration test
with CTest. Build the `mpv_memory_subtitles_fake_asr` target, then run
`ctest --test-dir build-asr-memory -R mpv-memory-subtitles --output-on-failure`.
CTest uses the libmpv SDK selected by CMake and runs the fake backend without
Whisper model weights.

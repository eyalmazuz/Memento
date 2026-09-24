# libmpv Memory-Subtitle Regression

The CTest integration checks the in-memory subtitle command API against an
installed libmpv SDK. It covers track creation/removal, cue IDs and mutations,
`sub-lines` and `sub-text`, observed property events, file-change cleanup, and
the raw-cue timing contract under `sub-speed=0.5` and `sub-delay=0.25`. It also
drives a deterministic fake segment through `MpvController`, verifies valid
WAV extraction and in-flight cancellation cleanup, and checks that repeated
decode failure from the real `AsrController` remains paused through delayed,
high-speed position updates. An explicit seek must trigger exactly one new
bounded retry. The failure test uses an empty temporary model file, disables
GPU/flash-attention fallback, and verifies Qt heartbeat events continue; it
does not load the user's model or exercise successful Whisper inference. The
audio/video fixture and XDG profile directories are created under a temporary
directory.

Run it on Linux with a C compiler, FFmpeg, and the SDK prefix set explicitly:

```sh
MPV_SDK=/path/to/libmpv-sdk sh tests/mpv-memory-subtitles/run.sh
```

Build the `mpv_memory_subtitles_fake_asr` and `asr_controller_retry_test`
targets, then run `ctest --test-dir build-full-release -R
mpv-memory-subtitles --output-on-failure`. CTest uses the SDK selected by
CMake and caps the combined harness at 30 seconds. The standalone `run.sh`
form accepts the built executables as arguments and runs the controller test
directly against its generated media fixture.

#!/bin/sh
set -eu

test_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
mpv_sdk=${MPV_SDK:?Set MPV_SDK to the installed libmpv SDK prefix}
mpv_include_dir=${MPV_INCLUDE_DIR:-"$mpv_sdk/include"}
mpv_library_dir=${MPV_LIBRARY_DIR:-"$mpv_sdk/lib"}
fake_asr_test=${1:-}
asr_controller_test=${2:-}
if [ -n "$fake_asr_test" ] && [ ! -x "$fake_asr_test" ]; then
    if [ -x "build-asr-memory/$fake_asr_test" ]; then
        fake_asr_test="build-asr-memory/$fake_asr_test"
    elif [ -x "build/$fake_asr_test" ]; then
        fake_asr_test="build/$fake_asr_test"
    fi
fi
if [ -n "$asr_controller_test" ] && [ ! -x "$asr_controller_test" ]; then
    if [ -x "build-asr-memory/$asr_controller_test" ]; then
        asr_controller_test="build-asr-memory/$asr_controller_test"
    elif [ -x "build/$asr_controller_test" ]; then
        asr_controller_test="build/$asr_controller_test"
    fi
fi
tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir"' EXIT HUP INT TERM
profile_dir="$tmp_dir/profile"
library_path="$mpv_library_dir${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
mkdir -p \
    "$profile_dir/home" \
    "$profile_dir/config" \
    "$profile_dir/data" \
    "$profile_dir/cache" \
    "$profile_dir/state" \
    "$profile_dir/runtime" \
    "$profile_dir/tmp"
chmod 700 "$profile_dir/runtime"

run_isolated()
{
    env \
        HOME="$profile_dir/home" \
        XDG_CONFIG_HOME="$profile_dir/config" \
        XDG_DATA_HOME="$profile_dir/data" \
        XDG_CACHE_HOME="$profile_dir/cache" \
        XDG_STATE_HOME="$profile_dir/state" \
        XDG_RUNTIME_DIR="$profile_dir/runtime" \
        TMPDIR="$profile_dir/tmp" \
        LD_LIBRARY_PATH="$library_path" \
        QT_QPA_PLATFORM=${QT_QPA_PLATFORM:-offscreen} \
        "$@"
}

test -f "$mpv_include_dir/mpv/client.h"
test -f "$mpv_library_dir/libmpv.so"

ffmpeg -nostdin -hide_banner -loglevel error \
    -f lavfi -i 'color=c=black:s=320x240:r=10:duration=30' \
    -f lavfi -i 'sine=frequency=440:sample_rate=48000:duration=30' \
    -c:v mpeg4 -q:v 10 -pix_fmt yuv420p \
    -c:a aac -shortest "$tmp_dir/test.mp4"

cc -std=c11 -Wall -Wextra -Werror \
    -I "$mpv_include_dir" \
    "$test_dir/client.c" \
    -L "$mpv_library_dir" \
    -Wl,-rpath,"$mpv_library_dir" \
    -lmpv \
    -o "$tmp_dir/mpv-memory-subtitles-test"

run_isolated "$tmp_dir/mpv-memory-subtitles-test" "$tmp_dir/test.mp4"

if [ -n "$fake_asr_test" ]; then
    run_isolated "$fake_asr_test" "$tmp_dir/test.mp4"
fi

if [ -n "$asr_controller_test" ]; then
    run_isolated "$asr_controller_test" "$tmp_dir/test.mp4"
fi

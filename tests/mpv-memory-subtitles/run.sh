#!/bin/sh
set -eu

test_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
mpv_sdk=${MPV_SDK:?Set MPV_SDK to the installed libmpv SDK prefix}
mpv_include_dir=${MPV_INCLUDE_DIR:-"$mpv_sdk/include"}
mpv_library_dir=${MPV_LIBRARY_DIR:-"$mpv_sdk/lib"}
fake_asr_test=${1:-}
if [ -n "$fake_asr_test" ] && [ ! -x "$fake_asr_test" ]; then
    if [ -x "build-asr-memory/$fake_asr_test" ]; then
        fake_asr_test="build-asr-memory/$fake_asr_test"
    elif [ -x "build/$fake_asr_test" ]; then
        fake_asr_test="build/$fake_asr_test"
    fi
fi
tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir"' EXIT HUP INT TERM

test -f "$mpv_include_dir/mpv/client.h"
test -f "$mpv_library_dir/libmpv.so"

ffmpeg -nostdin -hide_banner -loglevel error \
    -f lavfi -i 'color=c=black:s=320x240:r=10' \
    -t 3 -c:v mpeg4 -q:v 10 -pix_fmt yuv420p "$tmp_dir/test.mp4"

cc -std=c11 -Wall -Wextra -Werror \
    -I "$mpv_include_dir" \
    "$test_dir/client.c" \
    -L "$mpv_library_dir" \
    -Wl,-rpath,"$mpv_library_dir" \
    -lmpv \
    -o "$tmp_dir/mpv-memory-subtitles-test"

LD_LIBRARY_PATH="$mpv_library_dir${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}" \
    "$tmp_dir/mpv-memory-subtitles-test" "$tmp_dir/test.mp4"

if [ -n "$fake_asr_test" ]; then
    LD_LIBRARY_PATH="$mpv_library_dir${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}" \
        QT_QPA_PLATFORM=${QT_QPA_PLATFORM:-offscreen} \
        "$fake_asr_test" "$tmp_dir/test.mp4"
fi

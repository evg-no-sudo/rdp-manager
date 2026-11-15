#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
prefix="$root/.deps/freerdp-h264"
export PKG_CONFIG_PATH="$prefix/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
export LD_LIBRARY_PATH="$prefix/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
[ -f "$prefix/lib/pkgconfig/freerdp3.pc" ] || { printf 'Run sh scripts/build-h264.sh first\n' >&2; exit 1; }
fixture=$(mktemp -d /tmp/rdp-manager-h264.XXXXXXXX)
trap 'rm -rf "$fixture"' EXIT HUP INT TERM
ffmpeg -hide_banner -loglevel error -f lavfi -i color=c=red:s=64x64:r=25 \
    -frames:v 1 -c:v libx264 -pix_fmt yuv420p -tune zerolatency -f h264 "$fixture/red.h264"
cc -std=gnu11 -Wno-deprecated-declarations "$root/native/test_h264.c" \
    -o "$fixture/decoder" $(pkg-config --cflags --libs freerdp3 winpr3)
"$fixture/decoder" "$fixture/red.h264" 0
# A missing render node must still produce a correct software-decoded frame.
FREERDP_HWCTX_TYPE=vaapi FREERDP_HWCTX_DEVICE="$fixture/missing-render-node" \
    "$fixture/decoder" "$fixture/red.h264" 1 > "$fixture/fallback.log" 2>&1 || { cat "$fixture/fallback.log"; exit 1; }
rg 'red-frame decode OK' "$fixture/fallback.log"
if [ "${RDP_MANAGER_TEST_GPU:-0}" = 1 ]; then
    FREERDP_HWCTX_TYPE=vaapi "$fixture/decoder" "$fixture/red.h264" 1 > "$fixture/gpu.log" 2>&1 || { cat "$fixture/gpu.log"; exit 1; }
    cat "$fixture/gpu.log"
    rg -q 'Using vaapi .*accelerated H264 decoding' "$fixture/gpu.log"
fi

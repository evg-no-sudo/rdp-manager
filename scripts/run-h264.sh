#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
prefix="$root/.deps/freerdp-h264"
if [ ! -f "$prefix/lib/pkgconfig/freerdp-client3.pc" ]; then
    printf 'Build the private engine first: sh scripts/build-h264.sh\n' >&2
    exit 1
fi
export PKG_CONFIG_PATH="$prefix/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
export LD_LIBRARY_PATH="$prefix/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
export RDP_MANAGER_ICON_PATH="$root/data/icons"
export CARGO_TARGET_DIR="$root/target/h264"
. "$root/scripts/h264-backend.sh"
backend=${RDP_MANAGER_H264_BACKEND:-auto}
device=${RDP_MANAGER_RENDER_DEVICE:-${FREERDP_HWCTX_DEVICE:-}}
if [ "${RDP_MANAGER_H264_DECODER:-}" = software ]; then backend=software; fi
if [ "$backend" = auto ]; then
    if [ -n "${FREERDP_HWCTX_TYPE:-}" ]; then
        backend=$FREERDP_HWCTX_TYPE
    elif [ -n "$device" ]; then
        backend=vaapi
    else
        selection=$(rdp_manager_auto_backend /sys/class/drm /dev/dri)
        backend=${selection%% *}
        case "$selection" in *' '*) device=${selection#* } ;; esac
    fi
fi
case "$backend" in
    software) export RDP_MANAGER_H264_DECODER=software ;;
    vaapi|cuda)
        export FREERDP_HWCTX_TYPE="$backend"
        if [ "$backend" = vaapi ] && [ -n "$device" ]; then export FREERDP_HWCTX_DEVICE="$device"; fi
        ;;
    *) printf 'Unknown H.264 backend: %s (auto, software, vaapi, cuda)\n' "$backend" >&2; exit 1 ;;
esac
printf '[rdp-decoder] requested=%s device=%s (actual decoder depends on server, codec and driver)\n' "$backend" "${device:-default}" >&2
cd "$root"
exec cargo run --release --locked -- "$@"

#!/bin/sh
set -eu
rdp_launcher_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
. "$rdp_launcher_dir/h264-backend.sh"
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
exec "$@"

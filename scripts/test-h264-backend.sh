#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
. "$root/scripts/h264-backend.sh"
fixture=$(mktemp -d /tmp/rdp-manager-gpu-selection.XXXXXXXX)
trap 'rm -rf "$fixture"' EXIT HUP INT TERM
mkdir -p "$fixture/sys/renderD128/device" "$fixture/dev"
[ "$(rdp_manager_auto_backend "$fixture/sys" "$fixture/dev")" = software ]
touch "$fixture/dev/renderD128"
for vendor in 0x1002 0x8086; do
    printf '%s\n' "$vendor" > "$fixture/sys/renderD128/device/vendor"
    [ "$(rdp_manager_auto_backend "$fixture/sys" "$fixture/dev")" = "vaapi $fixture/dev/renderD128" ]
done
printf '0x10de\n' > "$fixture/sys/renderD128/device/vendor"
[ "$(rdp_manager_auto_backend "$fixture/sys" "$fixture/dev")" = cuda ]
mkdir -p "$fixture/sys/renderD129/device"
printf '0x8086\n' > "$fixture/sys/renderD129/device/vendor"
touch "$fixture/dev/renderD129"
[ "$(rdp_manager_auto_backend "$fixture/sys" "$fixture/dev")" = "vaapi $fixture/dev/renderD129" ]
printf 'GPU selection: AMD, Intel, NVIDIA, hybrid and no device OK\n'

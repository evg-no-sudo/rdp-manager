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
cd "$root"
exec sh "$root/scripts/launch-h264.sh" cargo run --release --locked -- "$@"

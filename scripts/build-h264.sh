#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
revision=97eff6ddfbbfaa8e42a126d362686bd001ff8bdc
source_dir="$root/.deps/src/FreeRDP-3.32.2"
build_dir="$root/.deps/build/FreeRDP-3.32.2"
prefix="$root/.deps/freerdp-h264"
for package in libavcodec libavformat libavutil libswscale libswresample openssl json-c fuse3 libpulse; do
    if ! pkg-config --exists "$package"; then
        printf 'Missing build dependency: %s\n' "$package" >&2
        printf 'Install: build-essential cmake git pkg-config libavcodec-dev libavformat-dev libavutil-dev libswscale-dev libswresample-dev libssl-dev libjson-c-dev libfuse3-dev libpulse-dev\n' >&2
        exit 1
    fi
done
if [ ! -d "$source_dir/.git" ]; then
    mkdir -p "$source_dir"
    git -C "$source_dir" init -q
    git -C "$source_dir" remote add origin https://github.com/FreeRDP/FreeRDP.git
fi
if ! git -C "$source_dir" cat-file -e "$revision^{commit}" 2>/dev/null; then
    git -C "$source_dir" fetch --depth=1 origin "$revision"
fi
if [ -n "$(git -C "$source_dir" status --porcelain --untracked-files=no)" ]; then
    printf 'Local edits in %s; refusing to replace or build altered sources.\n' "$source_dir" >&2
    exit 1
fi
git -C "$source_dir" -c advice.detachedHead=false checkout --detach "$revision"
[ "$(git -C "$source_dir" rev-parse HEAD)" = "$revision" ]
patch="$root/patches/freerdp-clipboard-mounts.patch"
git -C "$source_dir" apply "$patch"
trap 'git -C "$source_dir" apply -R "$patch"' EXIT
cmake -S "$source_dir" -B "$build_dir" \
    -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX="$prefix" \
    -DCMAKE_INSTALL_LIBDIR=lib -DCMAKE_INSTALL_RPATH='$ORIGIN' \
    -DBUILD_SHARED_LIBS=ON -DBUILD_TESTING=OFF -DWITH_SAMPLE=OFF \
    -DWITH_CLIENT=OFF -DWITH_CLIENT_COMMON=ON -DWITH_SERVER=ON \
    -DWITH_CHANNELS=ON -DWITH_CLIENT_CHANNELS=ON -DWITH_SERVER_CHANNELS=ON \
    -DWITH_X11=OFF -DWITH_WAYLAND=OFF -DWITH_CLIENT_SDL=OFF \
    -DWITH_FFMPEG=ON -DWITH_VIDEO_FFMPEG=ON -DWITH_FFMPEG_HWACCEL=ON \
    -DWITH_SWSCALE=ON -DWITH_DSP_FFMPEG=OFF -DWITH_OPENH264=OFF \
    -DWITH_CUPS=OFF -DWITH_PCSC=OFF -DWITH_PULSE=ON -DWITH_ALSA=OFF \
    -DWITH_OSS=OFF -DWITH_FUSE=ON -DWITH_RDP_MANAGER_CLIPBOARD_MOUNTS=ON -DWITH_JPEG=OFF -DWITH_AAD=OFF \
    -DWITH_MANPAGES=OFF -DWITH_URIPARSER=OFF -DWITH_SYSTEMD=OFF
cmake --build "$build_dir" --parallel "${RDP_MANAGER_BUILD_JOBS:-4}"
cmake --install "$build_dir"
printf 'Private FreeRDP installed: %s\nRun: sh scripts/run-h264.sh\n' "$prefix"

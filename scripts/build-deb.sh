#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
prefix="$root/.deps/freerdp-h264"
cd "$root"
[ "$(dpkg --print-architecture)" = amd64 ] && [ "$(uname -m)" = x86_64 ]
test -f "$prefix/lib/pkgconfig/freerdp-client3.pc"
export PKG_CONFIG_PATH="$prefix/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
export CARGO_TARGET_DIR="$root/target/h264"
cargo build --release --locked
version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -1)
test -n "$version"
version="$version-12"
out="$root/target/dist"
mkdir -p "$out"
work=$(mktemp -d "$out/deb.XXXXXXXX")
trap 'rm -rf "$work"' EXIT
stage="$work/rdp-manager"
app="$stage/usr/lib/rdp-manager"
id=io.github.rdpmanager.RdpManager
install -d "$stage/DEBIAN" "$stage/usr/bin" "$app/lib" "$work/debian"
install -m 755 "$CARGO_TARGET_DIR/release/rdp-manager" "$app/rdp-manager"
install -m 644 scripts/launch-h264.sh scripts/h264-backend.sh "$app/"
for lib in libfreerdp-client3 libfreerdp3 libwinpr3; do
    install -m 644 "$prefix/lib/$lib.so.3" "$app/lib/$lib.so.3"
    printf '%s 3 rdp-manager\n' "$lib" >>"$work/debian/shlibs.local"
done
install -Dm644 "data/$id.desktop" "$stage/usr/share/applications/$id.desktop"
cp -R data/icons "$stage/usr/share/"
install -Dm644 "$root/.deps/src/FreeRDP-3.32.2/LICENSE" "$stage/usr/share/doc/rdp-manager/copyright.FreeRDP"
cat >"$stage/usr/bin/rdp-manager" <<'LAUNCHER'
#!/bin/sh
app=/usr/lib/rdp-manager
export LD_LIBRARY_PATH="$app/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
exec sh "$app/launch-h264.sh" "$app/rdp-manager" "$@"
LAUNCHER
chmod 755 "$stage/usr/bin/rdp-manager"
cat >"$work/debian/control" <<'CONTROL'
Source: rdp-manager
Section: net
Priority: optional
Maintainer: Linux RDP Manager contributors <hitttmannn@users.noreply.github.com>

Package: rdp-manager
Architecture: amd64
Description: Remote Desktop client for Linux
CONTROL
depends=$(cd "$work" && dpkg-shlibdeps -O -xrdp-manager -l"$app/lib" \
    "$app/rdp-manager" "$app/lib/"*.so.3)
depends=${depends#shlibs:Depends=}
test -n "$depends"
cat >"$stage/DEBIAN/control" <<CONTROL
Package: rdp-manager
Version: $version
Architecture: amd64
Section: net
Priority: optional
Maintainer: Linux RDP Manager contributors <hitttmannn@users.noreply.github.com>
Depends: $depends, libgtk-4-1 (>= 4.14), libadwaita-1-0 (>= 1.5), fuse3, hicolor-icon-theme
Recommends: gnome-keyring
Installed-Size: $(du -sk "$stage/usr" | cut -f1)
Homepage: https://github.com/hitttmannn/rdp-manager
Description: Remote Desktop client for Linux
 Linux RDP Manager provides RDP tabs, saved connection profiles,
 clipboard and drive redirection, and audio playback.
CONTROL
artifact="rdp-manager_${version}_amd64.deb"
dpkg-deb --root-owner-group --build "$stage" "$out/$artifact"
(cd "$out" && sha256sum "$artifact" >"$artifact.sha256")

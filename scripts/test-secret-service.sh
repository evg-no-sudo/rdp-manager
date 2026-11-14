#!/bin/sh
# Synthetic secrets only; private D-Bus and temporary XDG directories.
set -eu
cd "$(dirname "$0")/.."
test_root=$(mktemp -d /tmp/rdp-manager-secrets.XXXXXXXX)
trap 'rm -rf "$test_root"' EXIT HUP INT TERM
mkdir -m 700 "$test_root/data" "$test_root/config" "$test_root/runtime"
export XDG_DATA_HOME="$test_root/data"
export XDG_CONFIG_HOME="$test_root/config"
export XDG_RUNTIME_DIR="$test_root/runtime"
export RDP_MANAGER_SECRET_TEST=isolated
dbus-run-session -- sh -eu -c '
    printf "%s" "synthetic-keyring-unlock-value" | gnome-keyring-daemon --unlock --components=secrets --control-directory="$XDG_RUNTIME_DIR/keyring"
    cargo test --locked --offline isolated_secret_service_lifecycle -- --ignored
'

# Linux RDP Manager

[Русский](README.md) · [Roadmap](ROADMAP.en.md)

**Developer:** Evgeny Anishchenkov

A native RDP client built with Rust, GTK4, libadwaita and FreeRDP. The main tested environment is Linux Mint 22.3 with Cinnamon on X11, connecting to Windows RDP and Linux/xrdp servers. KDE Plasma, GNOME and native Wayland are target environments. Full coverage is still pending.

## Features

- SQLite profiles, search, history and collapsible folders. Sort connections within folders by successful connection count or by name. Selectively import RDP profiles and groups from Remmina, or duplicate profiles locally.
- Tabbed sessions, separate windows, and tab return without reconnecting. Tabs and controls share one row in full screen. **Ctrl+Shift+F11** toggles full screen and **Right Ctrl** releases keyboard capture.
- Dynamic resolution when supported by the server, graphics quality presets and H.264 hardware decoding when supported by the GPU and driver. [H.264 diagnostics](docs/H264.md)
- Bidirectional text and file clipboard, streamed file and folder transfer between RDP sessions, a sent-byte counter and optional drive redirection. Receiving clipboard files on Linux requires FUSE. Large transfer speeds depend on the server and network.
- Remote audio playback. Experimental microphone forwarding is enabled with `RDP_MANAGER_MICROPHONE=1`. It has been tested with xrdp. Windows compatibility needs more work.
- Local Secret Service password storage and server certificate verification with remembered trust. Manually exchange selected profiles in encrypted bundles via a folder, WebDAV/Nextcloud or SFTP. Bundles exclude RDP passwords and folder assignments. [Profile exchange](docs/TRANSFER.md) · [WebDAV](docs/WEBDAV.md) · [SFTP](docs/SFTP.md)
- System, light and dark themes. The interface uses Russian for a Russian system locale and English otherwise. Set a preferred language via **⋮ → Interface language**, then restart the app.

## Install

The first package targets Linux Mint 22.3, amd64. It includes the tested FreeRDP/WinPR 3.32.2 build, and APT resolves the remaining dependencies. After installation, look for **Linux RDP Manager** in the application menu.

```sh
sudo apt install ./target/dist/rdp-manager_0.1.0-33_amd64.deb
rdp-manager
```

## Build

Development requires Rust/Cargo, pkg-config and development packages for GTK4 ≥ 4.14, libadwaita ≥ 1.5, FreeRDP ≥ 3.25, SQLite, libsecret, OpenSSL and zlib. Password storage requires a Secret Service provider in the user's session.

```sh
cargo build --locked
cargo test --locked
```

Build the engine with H.264, FUSE and PulseAudio, run the app and build the DEB (requires `dpkg-dev`):

```sh
sh scripts/build-h264.sh
sh scripts/run-h264.sh
sh scripts/build-deb.sh
```

The package and SHA-256 file appear in `target/dist/`. The default profile database is `${XDG_DATA_HOME:-$HOME/.local/share}/rdp-manager/profiles.sqlite3`. See [testing](docs/TESTING.md) and the [roadmap](ROADMAP.en.md).

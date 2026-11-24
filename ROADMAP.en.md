# Roadmap

[Русский](ROADMAP.md) · [Project overview](README.en.md)

## Near-term priorities

1. Verify DEB installation, X11 and native Wayland across Cinnamon, GNOME and KDE Plasma. Test keyboard shortcuts, HiDPI, clipboard transfers, Secret Service and window behavior. Consider AppImage after checking dependencies across multiple distributions.
2. Add recovery after network interruptions with bounded retries and explicit cancellation. Test full screen across multiple monitors.
3. Benchmark responsiveness and large transfers on Windows RDS and xrdp against mstsc, FreeRDP and Remmina. Verify active hardware decoding on Intel, AMD and NVIDIA, then reduce frame copies and evaluate zero-copy rendering. [Performance plan](docs/PERFORMANCE.md)
4. Add on-demand sync, followed by optional background sync. Merge independent edits safely, surface conflicts, and propagate deletions and groups. Share the mechanism across folder, WebDAV and SFTP backends, then add SMB. Secrets stay on each device. [Sync requirements](docs/SYNC.md)
5. Add optional Kerberos SSO using a user ticket without joining the computer to the domain. Verify microphone forwarding on Windows, then consider printers, smart cards and USB.
6. Maintain Russian and English interface strings and both versions of the main documentation as features are added.

## After Linux stabilizes

Evaluate an Android version. Profile models, encryption, exchange format and merge logic can be shared. Adapt the interface, touch input, credential storage and session lifecycle separately. Verify clipboard files, background sync and Kerberos before claiming support.

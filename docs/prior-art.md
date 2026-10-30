# Phase 0: prior art (summary)

Full survey with a source link for every claim, checked against live repositories on 2026-10-30: [research/prior-art.md](research/prior-art.md). The deeper dives are:
- [Tether security](research/tether-mtls.md)
- [KDE virtual display + encoding + libinput](research/kde-display-encode.md)
- [Apple Pencil APIs](research/pencil-apis.md)
- [xtool / CI / SideStore](research/xtool-ci-sidestore.md)

| Project | What it does | Maintenance (2026-10-30) | License | Reuse for us |
|---|---|---|---|---|
| **Weylus** | Rust server + browser client: tablet input via Pointer Events, screen mirror via fMP4 over WebSocket | last release v0.11.4 (2021), 1 PR merged since 2025, maintainer asking for help | AGPL-3.0-or-later | Ideas only (uinput layout, hover-leave workaround). No hover distance, roll, stylus buttons or haptics; can't make a Wayland virtual output; server-listens model doesn't fit usbmux. |
| **Sunshine + Moonlight** | Game streaming host + clients; pen pressure, tilt, azimuth and hover via `LiSendPenEvent` | very active | GPL-3.0 | Ideas only (no B-frames, IDR on demand, `async_depth=1`, KWin `.desktop` permission trick). UDP can't cross usbmux; no roll, squeeze, double-tap or haptics; no Linux virtual display ("not planned"). A good **baseline to compare latency against**. |
| **inputtino / libvirtualhid** | uinput virtual devices incl. pen tablet (Sunshine's input layer) | active | MIT | Cross-checked conventions; no code copied (we use the `evdev` crate). |
| **KRdp / KPipeWire** | KDE RDP server; creates KWin virtual monitors; zero-copy VA-API encode | ships with Plasma 6.8.2 | LGPL (one file GPL, one Apache) | The **virtual-monitor pattern** (`zkde_screencast stream_virtual_output`) and encoder settings, as ideas. No pen support. |
| **Tether** | iPhone ↔ Linux clipboard, files, notifications, messages; Wi-Fi mTLS | v0.3.0, 2026-10-29 | MIT | Pinning and "pairing-messages-only" ideas. We avoid its port/service type and features. See [tether.md](tether.md). |
| **InkLink** | iPad + Rust host pen display (Windows/Linux), usbmux, uinput incl. roll | created 2026-10-28, single author | MIT | Same architecture; no TLS/auth, no hover distance, no haptics. No code taken; collaboration possible. |
| **usbmuxd / libimobiledevice** | USB transport to iOS devices | slow but alive (usbmuxd 1.1.1 in Arch) | GPL-2.0 daemon, LGPL libraries | We **talk to the daemon over its socket** (plist protocol), so nothing is linked. |
| **idevice** (Rust) | iOS device protocols incl. usbmuxd | active | MIT | Not needed: our usbmux client is about 150 lines. |
| **PeerTalk** | iOS↔Mac over usbmux (iOS app listens on loopback) | inactive since 2020 | MIT | Confirms the loopback-listener technique. |
| GfxTablet, Deskreen, RustDesk, wayvnc, spacedesk, Duet, Astropad, InkWand… | various | — | various | Nothing that covers Apple Pencil Pro + KDE Wayland. Details in the research notes. |

**Decision:** a new MIT OR Apache-2.0 project (Rust daemon + Swift UIKit app). Reasons and license check in [DECISIONS D1–D2](DECISIONS.md).

Main reasons against the shortest path, a native iOS client for Weylus:
- Almost all missing features are on the **server** side (hover distance, roll, buttons, haptics back-channel, KWin virtual output, usbmux, TLS).
- Upstream is effectively stalled.
- AGPL would apply to any code we took from it.

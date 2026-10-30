<img src="assets/app-icon.svg" width="72" height="72" alt="">

# sidecarforlinux

**Use an iPad with Apple Pencil Pro as a full pen tablet, and later a second display, for a Linux desktop.** Built for KDE Plasma on Wayland.

- **Pen (phase 1)**:
  - Every 240 Hz pencil sample
  - Pressure (8192 levels), tilt, **barrel roll**, **hover distance**
  - **Double-tap** and **squeeze** mapped to the eraser, stylus buttons or shortcuts
  - **Pencil Pro haptics** when the PC switches tools
  - Appears as a real tablet in libinput, KDE's *Drawing Tablet* settings and Krita
- **Virtual display (phase 2, experimental, untested on real hardware)**:
  - a KWin virtual monitor at the iPad's resolution
  - captured from PipeWire and encoded with VA-API H.264 (GStreamer)
  - streamed on a second TLS channel and shown with `AVSampleBufferDisplayLayer`
  - the pen pinned onto it
  - Run it with `sidecard run --video virtual`.
- **Transport**: USB-C through `usbmuxd` first, Wi-Fi with Bonjour as fallback.
- **Security**: mutual TLS 1.3 everywhere, pinned certificates, and a 6-digit code confirmed on both devices the first time.

> **Status (2026-10-30, first night):**
> - **Linux side:** implemented and tested in CI, including real `/dev/uinput`, evtest and libinput.
> - **iPad app:** builds in CI. Its networking core passes tests on macOS against the real Rust daemon over Apple's TLS stack. The app **hasn't run on an iPad yet**.
>
> See [docs/REPORT.md](docs/REPORT.md) for exactly what is tested, built-but-untested, or assumed.

## How it relates to other projects

| Project | What it does | Relationship |
|---|---|---|
| [Tether](https://github.com/zackb/tether) | clipboard, files, notifications, messages between iPhone and Linux | **Use it alongside this project.** We don't duplicate any of it ([docs/tether.md](docs/tether.md)). |
| [Weylus](https://github.com/H-M-H/Weylus) | browser-based tablet/mirror (AGPL, last release 2021) | Inspiration only: no hover, roll, buttons or haptics, no usbmux, no Wayland virtual output. |
| [Sunshine](https://github.com/LizardByte/Sunshine) + Moonlight | game streaming with basic pen support (GPL) | A good baseline for comparison today, but no roll, squeeze or haptics, UDP-only (no usbmux), and no Linux virtual display. |
| [InkLink](https://github.com/RakeRaven/inklink) | very similar idea (MIT, started 2026-10-28) | Independent. No shared code; collaboration is possible. |

Why a new project: [docs/DECISIONS.md#d1](docs/DECISIONS.md#d1-new-project-instead-of-a-client-for-weylus-or-sunshine). Full survey: [docs/research/prior-art.md](docs/research/prior-art.md).

## Layout

```
daemon/       Rust daemon `sidecard`: transport, TLS/pairing, uinput tablet, latency tool, fake iPad
protocol/     wire protocol: Rust crate, Swift package, golden test vectors
ios-app/      iPad app (Swift, UIKit), built with xtool (SwiftPM + xtool.yml)
scripts/      doctor.sh, setup-system.sh, install-daemon.sh, resign-ipad.sh, krita-wayland.sh, CI checks
docs/         architecture.md, protocol.md, DECISIONS.md, MORNING.md, REPORT.md, latency.md, phase2 design, research
```

## Setup on Arch Linux

The short version is below; the detailed, ordered checklist is [docs/MORNING.md](docs/MORNING.md).

```bash
git clone https://github.com/no-fy/sidecarforlinux.git && cd sidecarforlinux
scripts/doctor.sh            # read-only report of what's missing
scripts/setup-system.sh      # shows its plan, asks, then: packages (+ uinput group/udev rule if /dev/uinput isn't writable yet)
# log out and back in only if it added the uinput group
scripts/install-daemon.sh    # cargo build --release, installs ~/.local/bin/sidecard (+ optional user service)
scripts/krita-wayland.sh     # optional: Krita launcher in Wayland mode, so it sees the pen whenever the iPad connects
```

System changes made by `setup-system.sh` (nothing else is touched):
- packages `usbmuxd libimobiledevice evtest libinput`
- group `uinput`
- `/etc/udev/rules.d/60-sidecarforlinux-uinput.rules`
- `/etc/modules-load.d/sidecarforlinux.conf`
- your user added to `uinput`

No DKMS module is needed (`uinput` is in-tree), so the t2linux kernel is fine as is.

### Try the tablet without an iPad

```bash
sidecard gen-demo > /tmp/demo.jsonl
sidecard replay /tmp/demo.jsonl --settle 5   # a stroke with pressure, tilt, roll, hover, double-tap, squeeze
```

Open Krita's tablet tester, or **System Settings → Drawing Tablet**, while it runs.

## Install the iPad app

CI builds an **unsigned** `.ipa` on every push and publishes it as the rolling [`nightly`](https://github.com/no-fy/sidecarforlinux/releases/tag/nightly) pre-release. Sign it with your **free Apple ID** in one of two ways. Don't use both with the same Apple ID: each revokes the other's certificate.

**A. xtool on Arch (re-sign from the PC every 7 days)**

```bash
# once: xtool AppImage on PATH, `xtool auth login`, Developer Mode on the iPad
scripts/resign-ipad.sh     # downloads nightly, gets a fresh 7-day profile, signs, installs
```

**B. SideStore (refreshes on the iPad)**

Install SideStore with iloader, enable LocalDevVPN, then add `https://github.com/no-fy/sidecarforlinux/releases/download/nightly/SidecarPad.ipa` in SideStore.

| | xtool | SideStore |
|---|---|---|
| 7-day refresh | from the PC (iPad plugged in) | on the iPad (VPN on) |
| App slots used (of 3) | 1 | 2 |
| Where the Apple ID login lives | PC | iPad |

Free Apple ID limits: profiles expire after **7 days**, at most **3 sideloaded apps** per device, and **10 App IDs per 7 days**. The app needs **no restricted capability**: Bonjour uses only `NSBonjourServices` and `NSLocalNetworkUsageDescription`, and there are no entitlements.

### The 7-day routine

Free provisioning profiles expire after 7 days, after which the app won't launch.
- **xtool:** run `scripts/resign-ipad.sh` with the iPad plugged in. It reinstalls over the app and keeps its data and pairing.
- **SideStore:** open SideStore with LocalDevVPN on and refresh.

## Pairing

1. Open the **Sidecar** app on the iPad. USB connected, iPad unlocked.
2. On the PC, run `sidecard pair`.
3. Both screens show a **6-digit code**. If they match, accept on **both**.
4. From then on, `sidecard run` (or the user service) connects automatically.

Over Wi-Fi, an unknown PC can't even start pairing unless you tap ⚙︎ → **Allow pairing over Wi-Fi (2 min)** in the app. Then run `sidecard pair --transport wifi`.

Forget a pairing:
- PC: `sidecard unpair NAME`
- iPad: ⚙︎ → Forget paired PCs

## Daemon commands

```
sidecard run [--pair] [--transport auto|usb|wifi|tcp] [--record FILE] [--dry-run]
             [--video off|virtual|test-pattern|file:CLIP.h264]   # phase 2, experimental
sidecard display-probe                # phase 2: create a KWin virtual monitor, print its PipeWire node
sidecard pair                         # pair a new iPad (asks on this terminal)
sidecard latency --duration 30        # touch -> uinput latency histogram (docs/latency.md)
sidecard devices                      # iPads over usbmuxd and mDNS
sidecard replay FILE [--loop]         # inject a recording into a virtual tablet, no iPad needed (moves your cursor; --loop stops after 30 s)
sidecard fake-ipad [--replay FILE]    # pretend to be the iPad app (tests, demos)
sidecard gen-demo | info | unpair NAME | example-config
```

Configuration lives in `~/.config/sidecarforlinux/config.toml` (`sidecard example-config` prints the defaults). It covers:
- the mapped area
- `[kde] output` to pin the pen to a screen
- double-tap/squeeze actions (`eraser`, `stylus`, `stylus2`, `keys:CTRL+Z`, …)
- predicted touches
- the pressure floor

## Building

- **Daemon:** `cargo build --release -p sidecard`, then `cargo test --workspace`.
- **Protocol (Swift):** `cd protocol/swift && swift test`. It runs on Linux too.
- **iPad app:** `cd ios-app && xtool dev build --ipa` on a Mac, or let CI do it ([`.github/workflows/ios.yml`](.github/workflows/ios.yml)). The VM that wrote this never builds iOS locally.

CI runs:
- **Linux:**
  - fmt and clippy
  - unit tests
  - end-to-end tests: daemon ↔ fake iPad over TLS, plus a fake usbmuxd for the USB path
  - a real `/dev/uinput` test as root
  - evtest/libinput/udev checks on replayed pencil events (artifact `uinput-evidence`)
- **macOS:**
  - Swift protocol tests
  - the iPad networking layer (`SidecarNet`) tested against Apple's TLS stack **and against the real Rust daemon**
  - the unsigned xtool build

## Troubleshooting

| Symptom | Fix |
|---|---|
| `opening /dev/uinput: Permission denied` | `scripts/setup-system.sh`, then log out and in. Check with `scripts/doctor.sh`. |
| `usbmuxd Connect ... connection refused` | The Sidecar app must be **open in the foreground** on the iPad. |
| `no iOS device attached over USB` | Unlock the iPad and tap **Trust**. Try another cable or a port on the other side of the laptop. Check with `idevice_id -l`. |
| `not paired; run sidecard pair` | Pair once (above). |
| Tablet works but on the wrong screen | System Settings → Drawing Tablet → map to screen, or `[kde] output = "DP-1"` (names from `kscreen-doctor -o`). |
| Krita draws, but with even width and no eraser (its Tablet Tester says "Mouse …") | Krita is in its default X11 mode and was started before the pen existed. Run `scripts/krita-wayland.sh` once, or restart Krita after the iPad connects. |
| Light strokes don't draw | Raise `min_contact_pressure` (default 0.06; libinput's tip threshold is 5 %). |
| App won't launch after a week | The profile expired: re-sign (above). |
| Anything else | `sidecard -vv run`, `journalctl --user -u sidecard -f`, and `scripts/doctor.sh`. |

## License

Dual-licensed under **MIT OR Apache-2.0** ([LICENSE-MIT](LICENSE-MIT), [LICENSE-APACHE](LICENSE-APACHE)). No GPL/AGPL code is included; see [DECISIONS D1/D2](docs/DECISIONS.md).

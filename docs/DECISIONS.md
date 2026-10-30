# Decisions

Every structural choice: the options considered, what was picked, why, and how to undo it.
Status labels used across the docs: **tested** (exercised by an automated test or a
run I observed), **built but untested** (compiles, never run against real hardware),
**assumption** (believed from documentation, not verified).

Research notes with sources for everything below are in [`docs/research/`](research/).

---

## D1. New project instead of a client for Weylus or Sunshine

- **Options**
  1. Native iOS client for **Weylus**, contributed upstream.
  2. Native client for **Sunshine/Moonlight** (or extend Moonlight iOS).
  3. Fork **InkLink** (MIT, created 2026-10-28, same concept).
  4. New MIT/Apache project that reuses ideas, not GPL code.
- **Choice:** 4.
- **Why**
  - **Weylus** (AGPL-3.0, last release 2021, effectively unmaintained) speaks JSON over a WebSocket. Its server has no hover distance, barrel roll, stylus buttons or haptics back-channel. It can't create a virtual monitor on Wayland, and its server is the side that listens, but usbmux needs the iPad to be the listener. Nearly all the work would land in an upstream that isn't merging.
  - **Sunshine/Moonlight** (GPL-3.0) already sends pressure, tilt and hover, but not roll, squeeze, double-tap or haptics. Its streams are UDP, which usbmuxd cannot tunnel, and it has no Linux virtual display. Changing the pen protocol means coordinated changes across moonlight-common-c, Sunshine and every client.
  - **InkLink** is MIT and very close in architecture: iPad as server, usbmux, uinput with roll. But it is two days old with a single author, uses plain TCP with no authentication, and sends no hover distance and no haptics. We borrow nothing from its code. Collaborating later is an option (see MORNING.md).
- **License check:** no AGPL/GPL code was copied. Only ideas were taken from GPL projects: Sunshine's encoder settings and KRdp's virtual-output pattern. Talking to the `usbmuxd` daemon over its socket doesn't link GPL code.
- **Undo:** the wire protocol is isolated in `protocol/`. A Weylus or Moonlight client could be added as another transport later without touching the pen pipeline.

## D2. License: MIT OR Apache-2.0 (dual)

- **Options:** MIT, Apache-2.0, or both.
- **Choice:** dual `MIT OR Apache-2.0`. Files: `LICENSE-MIT`, `LICENSE-APACHE`.
- **Why:**
  - It's the Rust ecosystem convention.
  - The dependencies are MIT, Apache-2.0 or ISC (rustls, ring, evdev, tokio, rcgen, mdns-sd, plist, swift-crypto, swift-certificates, swift-asn1), so they're compatible either way.
  - Code can flow both ways with MIT projects such as InkLink or Tether.
  - Apache-2.0 adds the patent grant.
- **Undo:** relicensing to one of the two is always possible: every contributor already granted both.

## D3. Linux daemon in Rust

- **Options:** Python (fast to prototype), Rust.
- **Choice:** Rust (`daemon/`, binary `sidecard`).
- **Why:**
  - The pen path is latency-critical, and Rust avoids GC pauses.
  - Phase 2 needs PipeWire, VA-API and GStreamer bindings, and Rust has mature crates for all of them.
  - One static binary is easy to package for Arch.
  - The protocol is type-checked and shared with the tests and the fake iPad.
- **VM cost:** about 1.5 GB of toolchain and a 1–2 minute clean build on 4 cores, which is acceptable.
- **Lighter build:** rustls uses the `ring` provider rather than `aws-lc-rs`, which avoids cmake and a long C build.
- **Undo:** the protocol is language-neutral (spec plus golden vectors), so a Python prototype could talk to the same iPad app.

## D4. Own binary protocol, two TCP connections

- **Options:** adopt Weylus's WebSocket/JSON, Moonlight's protocol, InkLink's framing, or define our own.
- **Choice:** our own. Each frame is `length:u32le type:u8 payload`, little-endian. `Hello`/`HelloAck` carry a major/minor version and capability bits.
  - The **input** and **video** channels are **separate TCP+TLS connections**, so a 200 kB keyframe can never delay a pen sample (TCP head-of-line blocking).
  - **Forward compatibility:** unknown message types are skipped, trailing bytes in known messages are ignored, and `PenBatch` carries the per-sample size.
- **Why:** none of the existing protocols carry roll, hover distance, squeeze phases, haptics or end-to-end timestamps, and none support USB/usbmux on iOS.
- **Spec:** [`docs/protocol.md`](protocol.md).
- **Verification:** the Rust implementation generates `protocol/vectors/vectors.json`, and the Swift implementation is tested against the same bytes on Linux and macOS (**tested** in CI).
- **Undo:** bump the major version.

## D5. iPad is the server; ports 47811 (USB) and 47812 (Wi-Fi)

- **Why the iPad listens:** usbmuxd can only open connections *to* a port on the device.
- **USB:** the app listens on `127.0.0.1:47811`, and the daemon reaches it with usbmux `Connect`. PeerTalk and Appium use the same mechanism. No Local Network prompt is involved.
  - **Assumption:** usbmux-forwarded connections arrive on loopback on iPadOS 26/27. Documented by PeerTalk, still used by Appium in 2026. Not verified on your iPad.
- **Wi-Fi:** a second listener on 47812, advertised over Bonjour as `_sidecarlinux._tcp` with the certificate fingerprint in the TXT record. It never accepts loopback or cellular.
- **Port changed from 47800:** InkLink already uses 47800, and Tether uses 5134.
- **Undo:** change `DEFAULT_PORT` in `protocol/rust/src/lib.rs` and `protocol/swift/.../Messages.swift`.

## D6. Security: mutual TLS 1.3 + pinned self-signed certificates + verified pairing

- **Options**
  - Plain TCP over USB.
  - TLS-PSK.
  - Tether-style trust-on-first-use.
  - mTLS with a short-authentication-string (SAS) pairing.
- **Choice**
  - Both sides hold a long-lived self-signed **P-256** certificate:
    - Linux: rcgen, stored in `~/.config/sidecarforlinux/`, key mode 0600.
    - iPad: swift-certificates, stored in the keychain.
  - The TLS 1.3 handshake requires a client certificate.
  - After the handshake each side compares the peer's SHA-256 fingerprint with its pins.
  - **Pairing** runs inside the TLS session:
    1. The iPad commits to a nonce, `C = H(fp_ipad ‖ N_i)`.
    2. The daemon sends `N_d`.
    3. The iPad reveals `N_i`.
    4. Both show `SAS = H(fp_d ‖ fp_i ‖ N_d ‖ N_i) mod 10^6`, and the user confirms on **both** devices.
- **Why the commitment matters:** without it, a man-in-the-middle can try about 10^6 certificates offline until both screens show the same code. With it, the attacker gets exactly one guess per attempt. This is the structure of Bluetooth numeric comparison and ZRTP.
- **Why not copy Tether:** its approval shows only a name or a fingerprint prefix, with no code comparison (details in `research/tether-mtls.md`).
- **USB uses TLS too:** any app on the iPad can connect to loopback, so "USB" alone doesn't prove the peer is your PC.
- **Wi-Fi:** the TLS verify block rejects unknown certificates unless the user opened the 2-minute "Allow pairing over Wi-Fi" window. A stranger on the LAN can't even reach the pairing prompt.
- **Status:**
  - **Tested:** the daemon side and the protocol, in end-to-end tests against the fake iPad (refusal, pairing, rejection, reconnect, wrong identity).
  - **Built but untested:** the iPad side.
- **Undo:** `sidecard unpair NAME`, or "Forget paired PCs" in the app. Deleting `~/.config/sidecarforlinux/identity.*` forces a new identity.

## D7. Clocks and latency measurement

- **iPad timestamps:** nanoseconds since app launch, on the same base as `UITouch.timestamp`. They are not raw uptime: Apple's privacy rules discourage sending boot-time-derived values off the device.
- **Offset estimate:** the daemon pings at 10 Hz for the first second, then at 1 Hz. It uses the minimum-RTT exchange of the last 16 (error ≤ RTT/2).
- **What `sidecard latency` measures:** `UITouch.timestamp` → `write()` into uinput. It excludes the compositor, the application and the display. Glass-to-glass measurement needs phase 2 video plus a camera; see [`docs/latency.md`](latency.md).
- **Undo:** none needed; this is purely observational.

## D8. uinput device layout

| Item | Value | Why |
|---|---|---|
| Devices | `Sidecar Apple Pencil` (pen) plus `Sidecar Apple Pencil Keys`, created only if a gesture is mapped to keys | libinput expects tablets to have no keyboard keys |
| Bus / ID | `BUS_VIRTUAL`, 1209:0001 (the pid.codes test PID) | Honest bus type. Not a Wacom ID, so libwacom doesn't apply Wacom quirks. To change it, edit `sink.rs`. |
| Property | `INPUT_PROP_DIRECT` (configurable) | It's a screen tablet once phase 2 runs. It doesn't change KWin's mapping. |
| ABS_X/Y | 0..W, 0..H at **100 units/mm**. Size comes from the iPad surface in points at 132 pt/inch. | libinput rejects tablets without a resolution. Real size gives the right aspect in KDE. |
| ABS_PRESSURE | 0..8191 (8192 levels) | The spec asks for ≥4096 |
| Contact pressure floor | **6 %** | libinput decides tip-down from pressure ≥ 5 % of range (tip-up ≤ 1 %) and ignores BTN_TOUCH. Without a floor, a light pencil touch wouldn't draw. Configurable (`min_contact_pressure`). |
| ABS_TILT_X/Y | −90..90 **degrees**, resolution 57 units/rad | As the spec asked, and the Wacom convention. libinput reads the value as `value/57` rad, i.e. 0.5 % too large (60° shows as 60.3°). Centidegrees with resolution 5730 would be exact; this is a one-line change in `tablet.rs`. |
| Tilt formula | `tiltX = atan2(cos(az)·cos(alt), sin(alt))`, `tiltY = atan2(sin(az)·cos(alt), sin(alt))` | Equal to the spec's `atan(cos(az)/tan(alt))` away from alt = 0 (unit-tested), but with no division by zero when the pencil lies flat |
| ABS_Z (roll) | 0..3599 tenths of a degree, sent as `(roll − 90°) mod 360` | libinput turns ABS_Z into rotation as `v/3600·360 + 90°` (Art Pen convention). Pre-compensating makes apps see rotation = roll (unit-tested against libinput's formula). Applications then see degrees with no offset. |
| ABS_DISTANCE | 0..1023 = zOffset 0..1 | Hover height. Having ABS_DISTANCE also turns off libinput's pressure-offset guessing. |
| Buttons | BTN_TOOL_PEN, BTN_TOOL_RUBBER, BTN_TOUCH, BTN_STYLUS, BTN_STYLUS2, BTN_STYLUS3 | Eraser toggle plus three barrel buttons |

**Proximity rules:**
- Hover means `BTN_TOOL_PEN=1` with `BTN_TOUCH=0`.
- On proximity-in, the axes are sent first and the tool event last, in the same frame.
- On tip-up and proximity-out, they go in separate frames.
- A stylus button pressed while the pencil is out of range gets a brief synthetic proximity, because libinput drops buttons otherwise.
- Tested: unit tests, a real-uinput CI test that reads the events back, and CI runs of evtest and libinput.

## D9. Default gesture mapping

- **Double-tap** toggles the **eraser** (BTN_TOOL_RUBBER). This mirrors the iPadOS default, and Krita switches to its eraser preset automatically.
- **Squeeze** clicks **BTN_STYLUS2** when released. Qt maps BTN_STYLUS2 to the right button, which opens Krita's pop-up palette, mirroring Apple's squeeze palette.
- Both are configurable: `none | eraser | stylus | stylus2 | stylus3 | keys:CTRL+Z`. Squeeze also has `squeeze_mode = click|hold`.
- When the tool changes, the daemon sends a **haptic** to the Pencil Pro.
- **Assumption:** Pencil Pro haptics may only fire while the pencil touches the screen. Apple doesn't document it; on-device testing needed.
- **Undo:** edit `[pencil]` in `config.toml`.

## D10. Predicted touches off, estimated-property updates ignored

- **Predicted touches:** they can't be retracted once written to uinput, so the app only sends them when "Send predicted touches" is enabled, and the daemon also ignores them unless `use_predicted = true`.
- **Estimated-property updates** (`touchesEstimatedPropertiesUpdated`): these refinements arrive after the sample was injected. Re-sending would add latency or retroactive corrections, so v1 injects the first estimate and flags it (`PRESSURE_ESTIMATED`/`ANGLES_ESTIMATED`).
- **Undo:** a later protocol minor version can add an update message.

## D11. Screen/region mapping

- **The `area` option** (daemon side, in `[tablet]`) maps the iPad onto a sub-rectangle of the tablet's coordinate space. This is generic and compositor-independent; **tested**.
- **KDE output mapping:** `[kde] output = "DP-1"` sets KWin's `org.kde.KWin.InputDevice.outputName` over D-Bus using `busctl`, with no D-Bus crate added. **Built but untested** (no KWin in CI).
- **Fallback:** KDE's System Settings → Drawing Tablet page lists the device and can map it.
- **Undo:** leave `output` empty.

## D12. iPad app: UIKit, scene lifecycle, iOS 17.5 minimum

- **Minimum iOS 17.5:** it's the first release with Pencil Pro roll, squeeze, hover pose and canvas haptics. Every Pencil-Pro-capable iPad runs it.
- **Scene lifecycle and a launch screen:** apps built with the iOS 27 SDK won't launch without them (TN3187).
- **Swift 5 language mode:** fewer strict-concurrency build breaks for Network.framework callbacks. The main-actor boundaries are annotated explicitly.
- **Landscape only** in phase 1, so the tablet's aspect stays fixed. A layout change sends `SurfaceInfo`, and the daemon then recreates the device.
- **Not used, so no free-account problem:** no entitlements, no App Groups, no push, no multicast entitlement. Bonjour needs only `NSBonjourServices` and `NSLocalNetworkUsageDescription` (TN3179).
- **Undo:** raise the minimum or switch the language mode in `ios-app/Package.swift`.

## D13. iPad build: xtool in CI, unsigned

- **Build:** `xtool dev build --configuration release --ipa` on the `macos-26` runner (Xcode 26.6, iOS 26.5 SDK). It needs no Apple ID. xtool is pinned to 1.24.0 and the download's SHA-256 is verified.
- **Distribution:** the `.ipa` is uploaded as an artifact and as a rolling **`nightly`** pre-release, which gives SideStore and `scripts/resign-ipad.sh` a stable URL.
- **No credentials in CI:** no Apple ID, certificates or profiles are ever stored there.
- **Fallback if xtool breaks:** `xcodebuild` on the same Swift package, or dual-boot macOS. Neither is used without asking you.
- **Undo:** the workflow is self-contained in `.github/workflows/ios.yml`.

## D14. Install paths: xtool on Arch or SideStore, but not both on the same Apple ID

- Each tool revokes the other's development certificate (research/xtool-ci-sidestore.md §5), so pick one per Apple ID. Details and trade-offs are in MORNING.md and the README.

## D15. Tests without an iPad

- **Fake iPad** (`sidecard fake-ipad`): implements the iPad's side of the protocol, including TLS, pairing and Pong, and replays JSONL recordings.
- **Synthetic recording** (`sidecard gen-demo`): exercises every signal.
- **Your own recordings:** `sidecard run --record file.jsonl` records real sessions, which become regression fixtures.
- **CI:** an ubuntu runner provides real `/dev/uinput` (verified by the probe workflow on 2026-10-30), plus evtest, libinput and udev.

## D16. Phase 2 direction (design only)

- **Virtual monitor:** KWin `zkde_screencast_unstable_v1.stream_virtual_output_with_description`, sized to the iPad. Falls back to the portal's VIRTUAL source.
- **Capture and encode:** PipeWire DMA-BUF, then VA-API H.264 (no B-frames, CBR, IDR on request), with x264 zerolatency as fallback.
- **Transport and decode:** AVCC framing on a separate TCP channel, decoded on the iPad with VideoToolbox.
- See [`docs/phase2-virtual-display.md`](phase2-virtual-display.md).

## D17. Coexisting with Tether

- **No overlap:** no clipboard, file or notification features. Different port (5134 vs 47811/47812) and different Bonjour type (`_tether._tcp` vs `_sidecarlinux._tcp`).
- **Ideas only:** taken from its MIT code. Integration notes are in [`docs/tether.md`](tether.md).

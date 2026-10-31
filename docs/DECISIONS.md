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

- **iPad networking split out:** the app's networking layer (`SidecarNet`: PeerServer, keychain identity, trust store, pairing state machine) is a separate target without UIKit.
  - On the macOS runner it is tested against Apple's real TLS stack: a Network.framework client plays the daemon.
  - It is also tested against the **real Rust daemon binary**, built for macOS in dry-run mode because uinput is Linux-only. This covers rustls ↔ Network.framework mutual TLS, the same 6-digit code computed in Rust and Swift, and pen samples injected.

- **Fake iPad** (`sidecard fake-ipad`): implements the iPad's side of the protocol, including TLS, pairing and Pong, and replays JSONL recordings.
- **Synthetic recording** (`sidecard gen-demo`): exercises every signal.
- **Your own recordings:** `sidecard run --record file.jsonl` records real sessions, which become regression fixtures.
- **CI:** an ubuntu runner provides real `/dev/uinput` (verified by the probe workflow on 2026-10-30), plus evtest, libinput and udev.

## D16. Phase 2 direction (design only)

- **Virtual monitor:** KWin `zkde_screencast_unstable_v1.stream_virtual_output_with_description`, sized to the iPad. Falls back to the portal's VIRTUAL source.
- **Capture and encode:** PipeWire DMA-BUF, then VA-API H.264 (no B-frames, CBR, IDR on request), with x264 zerolatency as fallback.
- **Transport and decode:** AVCC framing on a separate TCP channel, decoded on the iPad with VideoToolbox.
- See [`docs/phase2-virtual-display.md`](phase2-virtual-display.md).

## D18. Phase 2 encoder runs as a GStreamer subprocess

- **Options**
  1. gstreamer-rs bindings in the daemon.
  2. libav/FFmpeg bindings (as KPipeWire does).
  3. A `gst-launch-1.0` subprocess that writes H.264 Annex-B to stdout.
- **Choice:** 3.
- **Why**
  - It needs no `-dev` libraries or bindgen in the daemon build.
  - The pipeline is a single string, so you can test and tune it with `gst-launch-1.0` alone, which matters for driver quirks on your specific GPU.
  - It's easy to switch between VA-API and x264.
  - The pipe adds microseconds.
  - Access units are detected in the daemon (AUD or first-slice boundaries), so any encoder works.
- **Cost:** no programmatic IDR requests. Encoders produce a keyframe every 2 s instead.
- **Undo:** `videochan::VideoSource::Command` is one variant; a gstreamer-rs source can replace it without touching the protocol or the iPad.

## D17. Coexisting with Tether

- **No overlap:** no clipboard, file or notification features. Different port (5134 vs 47811/47812) and different Bonjour type (`_tether._tcp` vs `_sidecarlinux._tcp`).
- **Ideas only:** taken from its MIT code. Integration notes are in [`docs/tether.md`](tether.md).

## D19. The virtual tablet lives exactly as long as the iPad connection

- **Decision (yours, 2026-10-30):** `sidecard` creates the uinput tablet when an iPad session starts and destroys it when the session ends. It is not kept alive between connections.
- **Consequence, observed on your machine:** apps in X11 mode (XWayland) only pick up tablets that existed when they started. With Krita 6.0.4 in its default X11 mode, a pen created after Krita started arrived as a plain mouse (no pressure, no eraser). Created before Krita started, it arrived as a stylus with pressure and tilt.
- **What we do instead:** run Krita in native Wayland mode (`scripts/krita-wayland.sh`), where Qt picks up tablets as they appear. In Wayland mode, Krita's tester reported pen and eraser proximity from a pen created after Krita started.
- **Rejected:** a persistent device created at daemon start from the last iPad's geometry.

## D20. Two iPad video renderers, switchable at runtime

- **Why:** the second screen still felt laggy and the iPad's share (decode + display) was unmeasured. AVSampleBufferDisplayLayer can't report it: it decodes and displays inside iPadOS, with no per-frame callback.
- **Choice:** keep AVSampleBufferDisplayLayer, and add a Metal renderer selectable in ⚙︎ → Video renderer, so the same session can be compared A/B. The Metal renderer works like this:
  - `VTDecompressionSession`: asynchronous, real-time, no temporal processing, IOSurface NV12 output.
  - `CVMetalTextureCache` turns the NV12 planes into textures with no copy; the fragment shader converts YCbCr to RGB.
  - It draws into a `CAMetalLayer` with 2 drawables and `presentsWithTransaction = false`, and presents as soon as the GPU is done, with no display-link pacing. Only the newest decoded frame is drawn.
  - It reports `decoded_ns`, and the drawable's `presentedTime` as `displayed_ns`.
- **Prior art:** OpenDisplay (GPL-3.0, so ideas only, no code read or copied; [prior-art.md](prior-art.md)) arrived at the same design independently. Its header suspects AVSampleBufferDisplayLayer of buffering about one extra frame and says moonlight-qt has a Metal path for the same reason. That's unverified; with Metal selected, the "decoded -> on screen" figure shows what an immediate present costs, and the felt A/B difference tells us about the layer.
- **Default from 2026-10-31 until D24: Metal.** In your A/B on an iPad Air M4 (iPadOS 27, USB) Metal **felt better** than AVSampleBufferDisplayLayer. That's your subjective verdict; AVSampleBufferDisplayLayer reports no display time, so there's no number to compare. AVSampleBufferDisplayLayer stays selectable as a fallback. Fresh installs start on Metal; existing installs keep the saved choice.
- **Testability:** everything but the `UIView` (decoder, newest-frame slot, shader, renderer switch, feedback) lives in `SidecarNet` and runs in the macOS CI job.
- **Undo:** delete the Metal view and the switch; the AVSampleBufferDisplayLayer path is unchanged except that it now waits for a keyframe after a switch.

## D21. How the Metal renderer hands frames to the screen

- **Measured (your run, 2026-10-31):** iPad Air M4, iPadOS 27, USB, KDE custom mode 2360×1640@120 on Virtual-ipad (747–1135 frames per 10 s sent), round-1 Metal path:
  - decode 1.9 / 2.3 ms (p50/p95);
  - **decoded → on screen 38–46 / 50–60 ms**;
  - about 50 % of frames replaced by a newer one (≈ 110 fps into a 60 Hz panel).
- **Why that long** (an inference from Apple's documentation, not measured step by step):
  - Apple's "Understanding hitches in your app" describes the render loop: a change handed to Core Animation waits for the next commit deadline (a vsync), the render server composites it during the next refresh interval, and it is shown at the vsync after that.
  - On top of that, round 1 had 2 drawables and frames arriving faster than the panel. `nextDrawable()` blocked until the next vsync, and each drawable queued behind the previous one. A forum report describes the same effect: "present as soon as possible after all the other frames in the queue have been presented".
  - ≈ 8 ms average wait + 2 × 16.7 ms ≈ 42 ms, which matches the measurement.
- **Choice: three presentation modes**, in ⚙︎ → Video renderer, compared on the iPad itself (readout under the latency line) and in the daemon log:
  1. **Display link (default):** `CAMetalDisplayLink` on its own run-loop thread with `preferredFrameLatency = 1`. Apple: "The only acceptable values are 1.0 and 2.0" and "The final latency may be bigger if the system needs more time".
     - Once per refresh it draws the newest decoded frame into the drawable the link hands out, and draws nothing if no frame is new.
     - 3 drawables (Apple's default).
     - Nothing queues; whether latency 1 also shortens the compositor stage is what the test shows.
  2. **UI update link (iOS 18+):** `UIUpdateLink` with `wantsImmediatePresentation`. Apple: it "requests immediate rendering of the display frame after the last CATransaction commit for the current UI update… the rendered display frame presents one frame duration sooner… primarily useful for pencil-drawing apps".
     - The frame is drawn on the main thread in the `.beforeCATransactionCommit` phase and presented with that transaction (`presentsWithTransaction`; commit, `waitUntilScheduled`, `present()`, Apple's documented order), with `requiresContinuousUpdates`.
     - It never waits for a drawable on the main thread: it skips the refresh if two are still in flight.
     - **Assumption:** the immediate rendering also covers a Metal drawable presented in that transaction. The readout's "immediate %" (`UIUpdateInfo.isImmediatePresentationExpected`) shows whether iPadOS granted it.
  3. **Present at once:** round 1, unchanged, as a fallback and baseline.
- **Result (your run, 2026-10-31, round 2):** decoded → on screen p50, iPad Air M4, iPadOS 27:
  - **display link ≈ 36–39 ms**: the best of the three, and the default;
  - **UI update link ≈ 45–52 ms**: its documented "one frame duration sooner" did not show up here;
  - **present at once ≈ 45–48 ms**;
  - AVSampleBufferDisplayLayer: not measurable (no per-frame callback), and it felt worse earlier.
  So the display link saves ≈ 8–10 ms over round 1. What remains (≈ 2 refreshes) looks like the render server's commit-and-composite stages, which no presentation mode above removed.
- **Overlays:** Apple describes a single render server that composites every layer. I found no Apple documentation of an iOS path that bypasses it for a fullscreen Metal layer, so the pencil surface, ink and labels above the video stay. **Assumption:** they don't add a frame. Already done: the layer is opaque, `framebufferOnly`, and its drawable is the native 2360×1640, so no scaling pass.
- **Not used (yet):** `wantsLowLatencyEventDispatch`. It would also change how pencil touches are delivered, and the pen path is fine.
- **Sources:**
  - [CAMetalDisplayLink](https://developer.apple.com/documentation/quartzcore/cametaldisplaylink), [preferredFrameLatency](https://developer.apple.com/documentation/quartzcore/cametaldisplaylink/preferredframelatency), [metalDisplayLink(_:needsUpdate:)](https://developer.apple.com/documentation/quartzcore/cametaldisplaylinkdelegate/metaldisplaylink(_:needsupdate:))
  - [UIUpdateLink](https://developer.apple.com/documentation/uikit/uiupdatelink), [wantsImmediatePresentation](https://developer.apple.com/documentation/uikit/uiupdatelink/wantsimmediatepresentation)
  - [presentsWithTransaction](https://developer.apple.com/documentation/quartzcore/cametallayer/presentswithtransaction), [maximumDrawableCount](https://developer.apple.com/documentation/quartzcore/cametallayer/maximumdrawablecount), [presentedTime](https://developer.apple.com/documentation/metal/mtldrawable/presentedtime)
  - [Understanding hitches in your app](https://developer.apple.com/documentation/xcode/understanding-hitches-in-your-app)
  - forum thread [711033](https://developer.apple.com/forums/thread/711033) (not Apple staff)
  - All read 2026-10-31. I found no newer iPadOS 26/27 presentation API in Apple's API reference or in web searches. I didn't watch the WWDC25/26 Metal sessions; WWDC25 "Go further with Metal 4 games" has a present-pacing sample worth a look if these modes aren't enough.

## D22. Native capture and encode: pipewire-rs + raw libva

- **Context:** the GStreamer subprocess (D18) works on your machine, but pen input → encoded frame is ~33 ms and nothing tells us where it goes, and it can't make a keyframe on request.
- **Options**
  1. Keep the subprocess and tune it.
  2. `gstreamer-rs` in-process (appsrc/appsink).
  3. FFmpeg through `ffmpeg-next` (`h264_vaapi`).
  4. `cros-libva` / `cros-codecs` (ChromeOS, BSD-3).
  5. PipeWire through `pipewire-rs`, VA-API through our own narrow bindgen bindings.
- **Choice:** 5, selectable with `[video] backend = "native"`. `"gstreamer"` stays the default until the native path is proven on your machine. `"hybrid"` (native capture → raw frames → the old gst encoder) exists to test the capture half on its own.
- **Why**
  - It's zero copy: KWin's DMA-BUF is imported as a VA surface, converted BGRx → NV12 by VA video processing, and encoded. The PipeWire buffer goes back to KWin as soon as the conversion has read it.
  - Per-stage timing: KWin's capture tick (`spa_meta_header.pts`, CLOCK_MONOTONIC) → dequeued → converted → encoded → sent.
  - IDR on request, including re-encoding the last picture when the screen is idle.
  - Licences: libpipewire, libva and pipewire-rs are MIT. Arch's FFmpeg is built `--enable-gpl --enable-version3`, so option 3 would make the daemon GPLv3 when distributed. cros-libva's allowlist lacks the VPP and packed-header types (we'd write raw FFI anyway), and cros-codecs is a whole framework.
  - Capture and encode meet at a source-agnostic interface (DRM fourcc + modifier + plane fds, or SHM bytes; `native/frame.rs`). A non-PipeWire source, such as Wayfire's `wlr_export_dmabuf`, can feed the same encoder later.
- **Cost**
  - The default Linux build now needs **libclang** (bindgen) and the **libpipewire/libspa and libva headers**. On Arch that means the `clang`, `pipewire` and `libva` packages. `cargo build --no-default-features --features display` still builds without them, and macOS ignores the feature.
  - VA-API and KWin DMA-BUF can't run in the VM or on CI runners (no GPU), so the encoder is **built but untested** until you run it.
- **Undo:** set `backend = "gstreamer"`. The native code is behind the `native-video` cargo feature.
- **Capture cap at H.264 level 5.2 (2026-10-31):** with a 240 Hz Virtual-ipad mode, KWin offered 240 frames/s, the stream needed level 6.0, and Intel's encoder failed every first frame (`vaEndPicture … internal encoding error`), so the video restarted in a loop and the screen blinked in and out. The native capture now asks KWin for at most what level 5.2 allows at the screen's size (136 frames/s at 2360×1640; `[video] max_fps` can only lower it). A 240 Hz mode then still works: apps and KWin redraw at 240, the video goes out at 136. If a stream still needs more than 5.2, it is labelled 5.2 with a warning instead of failing. **Tested** in unit tests; on your machine **built but untested**.
- **Default since 2026-10-31 (round 3): `native`**, when the build has it (else `gstreamer`). On your machine it captures at 121 frames/s with Y-tiled zero copy, our own part is ≈ 9 ms, and you found pencil apps other than Krita "like usable compared to before" with it. In Krita the numbers came out about equal, because Krita's own drawing time dominates there. The example config no longer sets `backend`, so the build decides.
- **Status (your run, 2026-10-31, round 1): works on real hardware, slower than GStreamer.**
  - **Tested** on the Intel UHD 630 (iHD 26.3.5): H.264 High via EncSlice, packed headers accepted, KWin's DMA-BUF imported with no copy, 2360×1640 at level 5.1, and keyframe on request recovered a lost picture twice.
  - Slower than `gstreamer`: pen input → frame encoded ≈ 27–33 ms vs ≈ 10–11 ms. Stages: convert 5.4–6.9 ms, encode 6.0–6.8 ms, tick → sent ≈ 13.5 ms. KWin captured at **60** frames/s instead of the ~100 GStreamer got, because we capped the negotiated max framerate at `[video] fps`.
  - The negotiated buffer was **LINEAR** (modifier 0x0).
  - `vaapi-lp` failed: the low-power encoder offers constant QP only (modes 0x10) without the HuC firmware.
  - **AMD** (`va_device = renderD129`, Radeon Pro 5300M) can't import KWin's Intel-rendered buffers ("resource allocation failed"). The fallback to shared memory worked, but at 24 frames/s. AMD only makes sense if KWin itself renders on the AMD GPU.
- **Round 2 (built; the parts without a GPU are tested):**
  - capture as fast as the output refreshes (`[video] max_fps`, 0 = no cap);
  - conversion and encode submitted back to back, two frames in flight, stages measured on the GPU (plus a "ready" stage: KWin's own render finishing);
  - Y-/X-tiled modifiers offered before LINEAR on Intel;
  - constant QP when an encoder has no CBR (`[video] qp`);
  - `target_usage` mapped onto each driver's quality range (AMD has 32 levels; we sent 7, a slow high-quality preset);
  - configurations that can never work are reported once instead of retried forever.
  - Default stayed `gstreamer` until native was faster on your machine; it is the default since round 3 (above).
- **HuC (optional, untested):** Intel's media-driver README says "HuC firmware is necessary for AVC/HEVC/VP9/AV1 low power encoding bitrate control, including CBR, VBR", and that on CFL (Coffee Lake, your UHD 630) HuC loading has been off by default since kernel 4.15: set `i915.enable_guc=2`. That is a kernel parameter (`options i915 enable_guc=2` in `/etc/modprobe.d/`, root) plus a reboot. Not needed: EncSlice has CBR, and LP now runs with constant QP.
- **Details and sources:** [`research/native-video.md`](research/native-video.md).

## D23. Timing the AVSampleBufferDisplayLayer renderer

- **Why:** to compare it with the Metal modes (your round-2 numbers: display link ≈ 36–39 ms decoded → on screen, best of the three). The layer decodes and displays inside iPadOS and reports neither time per frame.
- **When a frame appears:** `AVSampleBufferVideoRenderer.displayedPixelBuffer()` (iOS 17.4+; Objective-C `copyDisplayedPixelBuffer`) returns a copy of the picture the layer shows now. A `CADisplayLink` on its own thread asks once per refresh, only while this renderer is visible. The refresh where a new picture first shows up is the frame's `displayed_ns`.
  - **Precision:** ±1 refresh (16.7 ms).
  - ~~**Assumption:** "displayed", as the layer reports it, means on the glass at that refresh.~~ **Disproven (your run, 2026-10-31):** received → "on screen" came out at 6–9 ms p50 (p95 10–18). That is less than one 60 Hz refresh, and iPadOS needs at least one refresh to composite a frame and another to light it up. So the copy shows the picture the layer has switched to, before the screen shows it. Metal's figure (`presentedTime`) is the glass, so the two numbers are **not comparable**. To the glass, this renderer is probably 1–2 refreshes later than its figure: roughly 23–42 ms, against Metal's ≈ 40. That is an estimate, not a measurement.
  - **Optimized compositing (your run):** about 50 % of the layer's frames while drawing fast, 100 % when idle. At 120 frames/s into a 60 Hz screen, about half the frames are replaced before they can be shown (the daemon line says the same), and they still count in the layer's total. So practically every frame that reached the screen went the "optimized" way, without being composited with the other UI. That is the likeliest reason this renderer could be a refresh (≈ 17 ms) quicker to the glass than Metal, whose frames go through the compositor. **Assumption**, since the glass can't be measured here; the copy format was `&8v0`, converted to `420v`.
- **Which frame it is:** the copy loses custom attachments (reported by another developer, [BD_to_AVP#1103](https://github.com/cbusillo/BD_to_AVP/issues/1103)), so attachments can't say. Instead the app also decodes every frame with VideoToolbox; that output is only fingerprinted, never shown. H.264 decoding is bit-exact by specification, so the luma of the layer's copy equals that of exactly one decoded frame (the oldest one, if consecutive frames look identical).
  - **Assumption:** the copy keeps 8-bit 4:2:0 luma unchanged.
  - **Fallback:** if no decode matches (copy converted or scaled), the first frame after a ≥ 150 ms pause owns the next screen change.
- **Reported as:** `VideoFeedback.displayed_ns` for frames seen on screen, 0 for frames never seen. `decoded_ns` stays 0, because decoding happens inside the layer and is unknowable. The daemon's `video on the iPad` line then fills in "received -> on screen" and "encoder out -> on screen" for this renderer, and gains "N shown" (its sample size).
- **The layer's own metrics:** `loadVideoPerformanceMetrics` (iOS 17.4+) goes into the on-iPad readout once a second:
  - frames and dropped frames;
  - **the share displayed "using optimized compositing"**, which Apple describes as "full screen frames rendered in a special power-efficient mode that didn't require compositing with other UI elements". That's the one way this renderer could beat Metal;
  - the average frame delay. The layer's own figure is relative to sample timestamps, which DisplayImmediately ignores, so treat it as informative only.
- **Reading the copy (fix, 2026-10-31):** on your iPad Air M4 (iPadOS 27) the copy is `&8v0`, Apple's lossless compressed NV12 (it was in a register of the crash report). Its "bytes per row" (75,776 for 2,360 pixels) describe compressed tiles, not rows. Reading it as plain NV12 ran off the end of its memory and crashed the app when this renderer was chosen.
  - Now the app never reads the copy with the CPU. `VTPixelTransferSession` copies it into a plain NV12 buffer of our own (same size and range), which is fingerprinted. The fingerprint reads only plain layouts it knows, and never past the buffer's size.
  - **Assumption:** the decompression gives back the luma unchanged (it is lossless). A macOS test does the round trip where the CI Mac supports `&8v0`.
  - If the copy can't be converted, the readout says "the layer's picture couldn't be read" and names its format.
  - The renderer choice is no longer saved: every launch starts on Metal.
- **Not measurable:** if `displayedPixelBuffer()` returns nil (Apple: protected content, nothing shown, "image unavailable", or a non-zero rate), the readout says so, the daemon line stays "not reported", and frames flow as before.
- **Cost:** a second hardware decode of every frame, plus a luma hash per frame, and per refresh a copy of the shown picture and a luma hash, only while this renderer is on screen.
  - **Assumption:** this doesn't measurably slow the layer on an M4.
- **Looked at, not used:** iOS 27's `AVSampleBufferVideoRenderer.Receiver` reports only decode failures and flush requests, not display times. CI builds with the iOS 26.5 SDK anyway.
- **Sources** (read 2026-10-31):
  - [displayedPixelBuffer()](https://developer.apple.com/documentation/avfoundation/avsamplebuffervideorenderer/displayedpixelbuffer()); its header comment (as mirrored in [objc2-av-foundation](https://docs.rs/crate/objc2-av-foundation/latest/source/src/generated/AVSampleBufferVideoRenderer.rs)) lists the nil cases
  - [loadVideoPerformanceMetrics](https://developer.apple.com/documentation/avfoundation/avsamplebuffervideorenderer/loadvideoperformancemetrics(completionhandler:)), [AVVideoPerformanceMetrics](https://developer.apple.com/documentation/avfoundation/avvideoperformancemetrics)
  - [Receiver](https://developer.apple.com/documentation/avfoundation/avsamplebuffervideorenderer/receiver)

## D24. AVSampleBufferDisplayLayer is the iPad's default renderer again; clean screen

- **Why:** your slow-motion video (2026-10-31, iPad Air M4, iPadOS 27) showed AVSampleBufferDisplayLayer **ahead of** Metal · display link. That's the only measurement of the glass we have, and it agrees with D23: practically every frame the layer shows skips the compositor ("optimized compositing"), while Metal's frames go through it.
  - Your earlier "Metal felt better" (D20) came before these fixes, by feel only.
- **Choice:**
  - AVSampleBufferDisplayLayer is the default, and the choice is saved again under a new key (`videoRenderer`), so an older saved Metal choice doesn't carry over.
  - Its timing (D23) is off by default, because it decodes every frame twice. It's a menu toggle (⚙︎ → Video renderer → Time AVSampleBufferDisplayLayer frames).
  - Metal stays selectable.
- **Clean screen** (⚙︎ → Clean screen while video is on, off by default, works with both renderers):
  - while video is on, the labels, the ⚙︎ button, the teal hover ring and the local ink are hidden;
  - a finger tap shows them for 6 s, and opening the menu restarts that.
  - The idea: with nothing drawn over the video, iPadOS needn't composite it with other UI, which could save a refresh (≈ 17 ms), mostly for Metal.
  - **Assumption, untested:** the layer's frames already skip the compositor with the overlays up, so the gain there may be nil. Judge by slow motion; with the layer's timing on, "optimized compositing" near the share of frames shown means nothing is left to gain.
- **Result (your test, 2026-10-31):** no noticeable difference with the clean screen. It stays as an option, off by default.
- **Undo:** select Metal in the menu; switch the clean screen off.

## D25. Finger touches become a touchscreen on the PC

- **Why:** with the iPad as a second screen, the mouse and the pencil work, but fingers did nothing, so pinch-zoom, two-finger pan and canvas rotation were missing (your request, 2026-10-31).
- **Choice:** the iPad sends finger touches (TouchBatch, protocol `0x13`), and the daemon creates a second virtual device, "Sidecar iPad Touch": a direct multitouch screen (Linux MT protocol B, 10 slots, `INPUT_PROP_DIRECT`) the size of the iPad, mapped to the same output as the pen. Apps that support touch then do their own gestures (Krita's canvas pan, zoom and rotate, browsers' pinch-zoom, scrolling).
  - **Not chosen:** turning gestures into keyboard shortcuts or scroll events on the iPad. That would work in fewer apps and need per-app tables.
- **Palm rejection, in the daemon** (testable there):
  - while the pencil is in proximity, new fingers are ignored and fingers already down are cancelled;
  - touches larger than `[touch] palm_radius_pt` (45 pt `UITouch.majorRadius`, an **assumption** to tune) are ignored, or cancelled when a finger spreads into one.
  - A cancel marks the slot `MT_TOOL_PALM` before ending it. libinput then reports a touch *cancel*, so apps drop the gesture instead of acting on a lift. CI checks this with the real libinput (`sidecard touch-demo`, `scripts/ci-uinput-check.sh`).
- **When:** only while the iPad shows the PC's screen (video on); as a plain tablet, fingers stay off. ⚙︎ → "Send finger touches to the PC" switches them off; `[touch] enabled = false` turns the device off on the PC. Both sides advertise the `touch` capability, so old daemons and old apps simply don't use it.
- **Clean screen:** a finger tap no longer shows the ⚙︎ (it goes to the PC); a finger in the top-right corner, where the ⚙︎ sits, does.
- **Lifetime:** like the pen (D19), the touchscreen exists only while the iPad is connected.
- **Pinning (fix, your first test):** the touchscreen first went to the laptop's screen. sidecard pinned only devices KWin reports as `tabletTool`, and KWin reports the touchscreen as `touch`. Both are pinned now (`outputName` over D-Bus). **Tested on your machine** (KWin 6.8): "works well now".
- **Undo:** `[touch] enabled = false`, or the iPad toggle.

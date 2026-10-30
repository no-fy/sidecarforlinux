# Phase 0: Prior-art research

**Project:** iPad + Apple Pencil Pro used as a full pen tablet (pressure, tilt, barrel roll, hover distance, squeeze, double-tap, haptics) and as a second display (a virtual monitor streamed as low-latency video) for a Linux PC. Target PC: Arch, KDE Plasma 6 on Wayland, AMD GPU, VA-API. Transport: USB-C through usbmuxd first, Wi-Fi as fallback. Clipboard, files and notifications are out of scope because Tether covers them.

**Date of research:** 2026-10-30. I checked every status, date and license against live sources: the GitHub REST API via `gh api`, the GitLab APIs at invent.kde.org, gitlab.freedesktop.org and gitlab.gnome.org, and raw source files. "Last commit" means the newest commit on the default branch at research time. I read source files one at a time and did not clone any repository.

---

## 0. Executive summary

| Project | Role for us | Last commit (default branch) | Last release | Archived | License (verified) | What we can reuse in an MIT/Apache project |
|---|---|---|---|---|---|---|
| **Weylus** (H-M-H/Weylus) | Browser-based tablet and mirror server | 2026-02-16 | v0.11.4, **2021-10-31** | no | **AGPL-3.0-or-later**. Contributions are BSD-3-Clause per the CONTRIBUTORS file. The GitHub API reports `NOASSERTION`. | Ideas only: protocol shape, hover workaround, uinput layout |
| **Sunshine** (LizardByte/Sunshine) | Game-stream host | 2026-10-28 | v2026.914.233613 (2026-09-15); pre-release 2026-10-28 | no | **GPL-3.0** | Ideas only. Interop with the Moonlight pen protocol is allowed. |
| **moonlight-common-c / moonlight-ios / moonlight-qt** | Client protocol library and clients | 2026-10-26 / 2026-09-26 / 2026-10-29 | n/a / no GitHub releases / v6.2.0 (2026-10-04) | no | **GPL-3.0** (all three) | Ideas only: pen field semantics, angle conversions |
| **inputtino** (games-on-whales) | uinput virtual-device library (formerly used by Sunshine) | 2026-07-13 | none | no | **MIT** | **Yes.** PenTablet uinput code. |
| **libvirtualhid** (LizardByte) | Sunshine's current input library | 2026-10-28 | none | no | **MIT** for the library and the Linux backend. LicenseRef-LizardByte-SAL-1.0 for the Windows driver and macOS broker. | **Yes**, the Linux uinput pen-tablet backend |
| **KRdp** (plasma/krdp) | KDE RDP server; creates KWin virtual monitors | 2026-10-29 | v6.8.2 (2026-10-27) | no | Mostly LGPL-2.1-only OR LGPL-3.0-only OR LicenseRef-KDE-Accepted-LGPL. `VideoStream.cpp` is GPL-2.0-or-later. | Ideas, or dynamic linking. The virtual-monitor pattern is the key idea. |
| **KPipeWire** (plasma/kpipewire) | Qt PipeWire capture and encode (VA-API through libav) | 2026-10-25 | v6.8.2 (2026-10-27) | no | LGPL-2.1-only OR LGPL-3.0-only OR KDE-Accepted-LGPL. `dmabufhandler.cpp` is Apache-2.0. | Dynamic-link it, or reimplement its VA-API filter chain (an idea) |
| **Tether** (zackb/tether) | iPhone continuity on Linux (clipboard, files, messages) | 2026-10-29 | v0.3.0 (2026-10-29) | no | **MIT** | Pairing and transport ideas. Must coexist with it (it uses port 5134). |
| **InkLink** (RakeRaven/inklink) | **Nearly our exact concept:** iPad app plus Rust host, usbmuxd, uinput pen including barrel roll | 2026-10-28 (repo created that day) | none | no | **MIT** | **Yes.** Fork it or borrow from it; very young (see §5.1). |
| **GfxTablet** | Android to uinput tablet | 2025-08-28 (README only) | none | **yes** | **MIT** | Trivial. Historical only. |
| **usbmuxd / libusbmuxd / libimobiledevice** | USB transport daemon and libraries | 2025-12-06 / 2025-09-07 / 2026-06-10 | usbmuxd 1.1.1 (2020) | no | GPL-2.0 (daemon); LGPL-2.1 (libraries); iproxy tool GPL-2.0-or-later | Use the **daemon** over its socket (no linking needed); talk its plist protocol directly |
| **idevice** (jkcoxson) | Rust iOS-device library with usbmuxd support | 2026-10-27 | v0.1.68 (2026-09-15) | no | **MIT** | **Yes** |
| **PeerTalk** (rsms) | iOS/macOS USB channel library | 2022-04-27 (last code 2020) | n/a | no | **MIT** | Ideas only (the iOS side is simply a TCP listener) |

**Main conclusions:**

1. **No existing open-source project delivers the whole feature set on KDE Wayland today.** Sunshine plus Moonlight iOS comes closest for the display and pen basics:
   - Moonlight iOS already sends Pencil pressure, tilt, azimuth and hover distance.
   - Sunshine injects these as a uinput tablet and captures KWin through the `zkde_screencast` protocol.

   What it lacks:
   - barrel roll, squeeze, double-tap and haptics;
   - creating Linux virtual displays (the maintainers closed that request as "not planned");
   - working over usbmuxd. Its streams are UDP, and usbmuxd only tunnels TCP.

2. **Weylus is effectively stalled.** It has one merged PR since 2025-01-01, 16 open PRs, and its last release was in 2021. It is AGPL. Its protocol is JSON pointer events over a WebSocket, with H.264 in fragmented MP4 over the same socket. Its pen device has no hover distance, no rotation and no stylus buttons, and there is no back-channel for haptics.

3. **KDE already offers every building block needed:**
   - `zkde_screencast_unstable_v1.stream_virtual_output` creates a virtual output and streams it through PipeWire. KRdp and krfb-virtualmonitor use it.
   - KWin's D-Bus `org.kde.KWin.InputDevice.outputName` pins a uinput tablet to one output.
   - KPipeWire's zero-copy chain is DMA-BUF → `hwmap` → `scale_vaapi` → `h264_vaapi`.
   - libei gained a stylus capability in main (unreleased "1.7"). No KWin EIS stylus support exists yet, so **uinput remains the injection path**.

4. **There are MIT-licensed shortcuts:** InkLink (MIT, created 2026-10-28) already implements the iPad-as-server, usbmuxd `Connect`, portal virtual-screen and uinput approach, including barrel roll on `ABS_Z`. inputtino and libvirtualhid (both MIT) have well-tested uinput pen-tablet code.

5. **Recommendation: option (b).** Build a new MIT/Apache-2.0 project: a Rust host plus a Swift iPad app, with the iPad as the TCP server reached over usbmuxd. Reuse MIT code (InkLink, inputtino/libvirtualhid, idevice, crates) and borrow only ideas from the GPL/AGPL projects. Use Sunshine plus Moonlight as an immediate baseline and latency benchmark. See §7.

---

## 1. Weylus (github.com/H-M-H/Weylus)

### 1.1 What it does and how it is built
- A Rust desktop server with an fltk GUI that serves a web page. A browser on the tablet acts as the client: it renders the screen through Media Source Extensions and sends Pointer Events back. ([Readme](https://github.com/H-M-H/Weylus/blob/master/Readme.md), [src/web.rs](https://github.com/H-M-H/Weylus/blob/master/src/web.rs), [ts/lib.ts](https://github.com/H-M-H/Weylus/blob/master/ts/lib.ts))
- The client is TypeScript compiled to `lib.js` and embedded in the binary ([ts/lib.ts](https://github.com/H-M-H/Weylus/blob/master/ts/lib.ts), [src/web.rs](https://github.com/H-M-H/Weylus/blob/master/src/web.rs)).
- **There is no native iOS client.** The issues "Native iOS and iPadOS apps" [#191](https://github.com/H-M-H/Weylus/issues/191) (2022) and "Native App" [#212](https://github.com/H-M-H/Weylus/issues/212) (2023) are still open.
  - The only third-party mobile client found is a Flutter "stylus-to-mouse" app ([ViscousPot/WeylusMouseReplacement](https://github.com/ViscousPot/WeylusMouseReplacement), last push 2025-12-15).
  - A GTK desktop client attempt was abandoned in 2023 ([OmegaRogue/weylus-desktop](https://github.com/OmegaRogue/weylus-desktop), AGPL-3.0).

### 1.2 Transport and protocol
- **Port and framing:**
  - One HTTP server, default bind `0.0.0.0:1701` ([src/config.rs L75-79](https://github.com/H-M-H/Weylus/blob/master/src/config.rs)).
  - It serves `/`, `/lib.js` and `/style.css`, and upgrades `/ws` to a WebSocket using `fastwebsockets` ([src/web.rs L94, L155-163](https://github.com/H-M-H/Weylus/blob/master/src/web.rs)).
  - An optional `access_code` is checked as a query parameter ([src/web.rs L93-101](https://github.com/H-M-H/Weylus/blob/master/src/web.rs)).
  - The README FAQ still mentions an old separate WebSocket port 9001 ([Readme](https://github.com/H-M-H/Weylus/blob/master/Readme.md)). Current code uses `/ws` on the web port.
- **Control messages:** WebSocket **text** frames carrying JSON. These are serde externally-tagged enums ([src/websocket.rs L521, L561](https://github.com/H-M-H/Weylus/blob/master/src/websocket.rs); [src/protocol.rs](https://github.com/H-M-H/Weylus/blob/master/src/protocol.rs)).
  - Inbound (client to server): `PointerEvent`, `WheelEvent`, `KeyboardEvent`, `GetCapturableList`, `Config(ClientConfiguration)`, `PauseVideo`, `ResumeVideo`, `RestartVideo`, `ChooseCustomInputAreas`.
  - Outbound (server to client): `CapturableList(Vec<String>)`, `NewVideo`, `ConfigOk`, `CustomInputAreas`, `ConfigError(String)`, `Error(String)`.
  - **There is no outbound message type that could carry haptics or other feedback.**
  - `ClientConfiguration` = {`uinput_support`, `capturable_id`, `capture_cursor`, `max_width`, `max_height`, `client_name`, `frame_rate`}. There is no bitrate field, no explicit keyframe request (only `RestartVideo`), and no latency statistics.
  - `PointerEvent` = {`event_type` ("pointerdown"/"pointermove"/…), `pointer_id`, `timestamp` u64, `is_primary`, `pointer_type` ("pen"/"touch"/"mouse"/""), `button`, `buttons` (bitflags PRIMARY, SECONDARY, AUXILARY, FOURTH, FIFTH, ERASER), `x`, `y` (f64), `pressure` (f64), `tilt_x`, `tilt_y`, `twist` (i32), `width`, `height`}. This mirrors the W3C PointerEvent ([src/protocol.rs](https://github.com/H-M-H/Weylus/blob/master/src/protocol.rs)).
  - The `movement_x` and `movement_y` fields were removed on 2026-02-15 ([commit 69b4941](https://github.com/H-M-H/Weylus/commit/69b4941a621024e5f06ef3b63bb615f99e6eb72b)). Issue "Unable to parse message" [#520](https://github.com/H-M-H/Weylus/issues/520) is open and PR [#521](https://github.com/H-M-H/Weylus/pull/521) touches the same fields. The protocol is unversioned.
  - The web client batches `getCoalescedEvents()` for `pointermove` ([ts/lib.ts L807](https://github.com/H-M-H/Weylus/blob/master/ts/lib.ts)).
- **Video:** WebSocket **binary** frames carrying **H.264 in fragmented MP4**:
  - `movflags=frag_custom+empty_moov+default_base_moof`, with "a new fragment on every frame for lowest latency" ([lib/encode_video.c L788, L862](https://github.com/H-M-H/Weylus/blob/master/lib/encode_video.c));
  - sent as `Frame::binary` ([src/websocket.rs L552-555](https://github.com/H-M-H/Weylus/blob/master/src/websocket.rs));
  - played through MSE, or `ManagedMediaSource` on iOS Safari ([ts/lib.ts L947](https://github.com/H-M-H/Weylus/blob/master/ts/lib.ts)).
- **Encryption:** none. The README recommends putting a TLS proxy such as hitch in front ([Readme "Encryption"](https://github.com/H-M-H/Weylus/blob/master/Readme.md)).

### 1.3 Pen and input injection on Linux
- It creates four uinput devices: "Weylus Stylus", "Weylus Mouse", "Weylus Touch" and "Weylus Keyboard". They use bus `BUS_VIRTUAL` with vendor and product `0x1701` ([lib/linux/uinput.c](https://github.com/H-M-H/Weylus/blob/master/lib/linux/uinput.c), [src/input/uinput_device.rs](https://github.com/H-M-H/Weylus/blob/master/src/input/uinput_device.rs)).
- **Stylus device** (`init_stylus` in [lib/linux/uinput.c](https://github.com/H-M-H/Weylus/blob/master/lib/linux/uinput.c)):
  - properties: `INPUT_PROP_DIRECT`;
  - keys: `BTN_TOOL_PEN`, `BTN_TOOL_RUBBER`, `BTN_TOUCH`;
  - `EV_MSC/MSC_TIMESTAMP`;
  - axes: `ABS_X` and `ABS_Y` 0..65535 (res 12); `ABS_PRESSURE` 0..65535 (res 12); `ABS_TILT_X` and `ABS_TILT_Y` −90..90 (res 12).
  - **There is no `ABS_DISTANCE`, no `ABS_Z` (rotation or barrel roll), and no `BTN_STYLUS`/`BTN_STYLUS2`.**
  - The `twist` field is received but **never injected**. In [uinput_device.rs](https://github.com/H-M-H/Weylus/blob/master/src/input/uinput_device.rs) only x, y, pressure, `tilt_x` and `tilt_y` are written.
  - Tilt goes in as raw degrees with resolution 12. That is a mismatch: libinput treats a non-zero tilt resolution as units per radian (§5.8).
- **Hover:** a pen move without contact sets `BTN_TOOL_PEN=1` with pressure 0. A workaround force-removes the pen after 50 ms when touch arrives, because some browsers never send a hover-leave ([uinput_device.rs ~L311-327](https://github.com/H-M-H/Weylus/blob/master/src/input/uinput_device.rs); commit "Workaround for hovering pens and touch rejection", 2024-10-18).
  - In 2021 Apple Pencil hover did not reach Safari as PointerEvents ([#85](https://github.com/H-M-H/Weylus/issues/85)). Hover is still reported as broken on Wayland in [#315](https://github.com/H-M-H/Weylus/issues/315) and [#336](https://github.com/H-M-H/Weylus/issues/336).
- **Touch:** a 5-slot multi-touch device with `ABS_MT_*` axes, pressure, touch major and minor ([uinput.c `init_touch`](https://github.com/H-M-H/Weylus/blob/master/lib/linux/uinput.c)).
- **Mapping to a monitor:**
  - On X11 it uses `xinput`-style mapping.
  - On Wayland it **skips** mapping ([uinput_device.rs ~L296-307](https://github.com/H-M-H/Weylus/blob/master/src/input/uinput_device.rs)). Coordinates are instead transformed by "custom input areas" that the user selects by hand (commit "Add selection of custom input areas on Wayland", 2024-10-11; [commit list](https://github.com/H-M-H/Weylus/commits/master)).
  - "Wayland with dual screens causes input to be relative to both monitors" is still open as [#229](https://github.com/H-M-H/Weylus/issues/229).

### 1.4 Wayland/KDE capture and virtual displays
- Wayland capture goes through the xdg-desktop-portal ScreenCast API, then the GStreamer `pipewiresrc` element, then an `appsink` ([src/capturable/pipewire.rs](https://github.com/H-M-H/Weylus/blob/master/src/capturable/pipewire.rs); [remote_desktop_dbus.rs](https://github.com/H-M-H/Weylus/blob/master/src/capturable/remote_desktop_dbus.rs)).
  - It requests `persist_mode=2` (L418), `multiple=true` (L449) and **all** `available_source_types` (L454). On KDE that list should include the portal's "Virtual Output" option, but this was not tested.
  - The RemoteDesktop portal is used "only on gnome" (commit 2024-10-16).
- Frames are **copied to CPU memory** (`always-copy=true`, BGRx/RGBx caps, `max-buffers=1`) and then handed to FFmpeg ([pipewire.rs L134-148](https://github.com/H-M-H/Weylus/blob/master/src/capturable/pipewire.rs)). There is **no DMA-BUF zero-copy** on master. Open PR [#560](https://github.com/H-M-H/Weylus/pull/560) (2026-07-29) adds DMA-BUF for niri.
- It **never creates virtual outputs.** The README's "second screen" section covers Intel Xorg `VirtualHeads`, dummy plugs, sway `create_output` and GNOME Mutter only ([Readme "Weylus as Second Screen"](https://github.com/H-M-H/Weylus/blob/master/Readme.md)).
- Wayland support is labelled "experimental" in the README. KDE uses `xdg-desktop-portal-kde`.

### 1.5 Video pipeline
- FFmpeg is statically linked ([Readme](https://github.com/H-M-H/Weylus/blob/master/Readme.md)). Settings from [lib/encode_video.c](https://github.com/H-M-H/Weylus/blob/master/lib/encode_video.c):
  - GOP 12 with B-frames 0 (L142-144);
  - VAAPI: `h264_vaapi` with `quality=7` and `qp=23` (L521-548), after a `hwupload,scale_vaapi` filter (L307), so it uploads from CPU memory;
  - NVENC: `p1`, `zerolatency`, `ull`, `cbr`, `delay=0` (L653-658);
  - libx264 fallback: `ultrafast`, `zerolatency`, crf 23 (L758-760).
- Hardware acceleration is **off by default** and enabled with `--try-vaapi` ([src/config.rs](https://github.com/H-M-H/Weylus/blob/master/src/config.rs); Readme "Hardware Acceleration").

### 1.6 Maintenance status
- Last commit 2026-02-16 (CI and dependency updates); last protocol change 2026-02-15 ([commits](https://github.com/H-M-H/Weylus/commits/master)).
- **Last release v0.11.4 on 2021-10-31** ([releases](https://github.com/H-M-H/Weylus/releases)). 9,728 stars, 166 open issues (GitHub API).
- **Only one PR merged since 2025-01-01:** [#487](https://github.com/H-M-H/Weylus/pull/487) "Fix ios video rendering", merged 2025-10-21. There are **16 open PRs**. Some date from 2024-09 ([#289](https://github.com/H-M-H/Weylus/pull/289), [#290](https://github.com/H-M-H/Weylus/pull/290), [#291](https://github.com/H-M-H/Weylus/pull/291)); others from 2026-09 ([#571](https://github.com/H-M-H/Weylus/pull/571)–[#574](https://github.com/H-M-H/Weylus/pull/574)). (`gh api search/issues …is:pr is:merged merged:>=2025-01-01`)
- The maintainer wrote in 2023 that he did "not have the time to maintain Weylus properly" ([#255](https://github.com/H-M-H/Weylus/issues/255)) and opened "Contributors / Maintainers wanted!" ([#287](https://github.com/H-M-H/Weylus/issues/287)).
- **Community fork:** [electronstudio/WeylusCommunityEdition](https://github.com/electronstudio/WeylusCommunityEdition) collects open PRs. Last push 2026-08-21; release 2026.5.22 published 2026-05-25. Same license.

### 1.7 License
- [LICENSE](https://github.com/H-M-H/Weylus/blob/master/LICENSE): "Weylus is licensed under the GNU Affero General Public License, version 3 or later. Contributed work is licensed under the 3-Clause BSD License."
- [CONTRIBUTORS](https://github.com/H-M-H/Weylus/blob/master/CONTRIBUTORS): contributors must agree to license their contributions under BSD-3-Clause.
- The GitHub API reports `NOASSERTION`.
- **Result:** the combined work, and H-M-H's own code, is AGPL-3.0+. **We cannot copy it into an MIT/Apache project.** Separating out the BSD-only contributor parts is impractical.

### 1.8 Relevant issues and PRs on iPad, Apple Pencil and USB
- Native iOS app: [#191](https://github.com/H-M-H/Weylus/issues/191) (open), [#212](https://github.com/H-M-H/Weylus/issues/212) (open).
  - In #212, contributor OmegaRogue (listed in CONTRIBUTORS) wrote that "the Weylus protocol is in dire need of a complete redesign".
  - Another commenter suggested merging Weylus into Moonlight.
- USB for iPad: "Leverage USB for Ipad" [#111](https://github.com/H-M-H/Weylus/issues/111) (open; the workaround is an Ethernet adapter); "USB Tethering for iOS" [#183](https://github.com/H-M-H/Weylus/issues/183) (open).
- iPad problems:
  - [#501](https://github.com/H-M-H/Weylus/issues/501) "Works on iPhone but not iPad"
  - [#537](https://github.com/H-M-H/Weylus/issues/537) "Issue with Safari compatibility on iPadOS 26.3.1"
  - [#314](https://github.com/H-M-H/Weylus/issues/314) "Drawing position is offset from pen-position"
  - [#338](https://github.com/H-M-H/Weylus/issues/338) black screen on iPad

### 1.9 Evaluation: a native iOS client for Weylus, contributed upstream
**Client effort is moderate. Server and protocol effort is large. Upstream acceptance risk is high.**

The client work:
- `URLSessionWebSocketTask` to `ws://host:1701/ws?access_code=…`.
- JSON `PointerEvent` messages built from `UITouch`. Apple Pencil pressure, tilt and azimuth map onto PointerEvent `pressure`, `tilt_x`, `tilt_y` and `twist`.
- A **small fMP4 box parser** that turns each `moof`/`mdat` into `CMSampleBuffer`s for `AVSampleBufferDisplayLayer` or VideoToolbox. AVFoundation does not consume a raw live fMP4 byte stream directly.
- Estimate: a few thousand lines of Swift for an MVP. The real gaps are on the server and in the protocol.

| Needed feature | Weylus today | Required change |
|---|---|---|
| Hover distance | No `ABS_DISTANCE`; PointerEvent has no distance field | New protocol field plus uinput axis |
| Barrel roll (Pencil Pro `rollAngle`) | `twist` exists in JSON but is **ignored** | Inject `ABS_Z`, in libinput's Art Pen convention (§5.8) |
| Side buttons, squeeze, double-tap | Only the ERASER bit (switches to rubber tool); no `BTN_STYLUS*` | Protocol messages; `BTN_STYLUS`/`BTN_STYLUS2` or key shortcuts |
| Haptics back-channel (PC to Pencil Pro) | No outbound feedback message | New outbound message type |
| Tilt units | Degrees with res 12, which libinput reads as radians | Fix the absinfo |
| USB / usbmuxd (**the PC must dial the iPad**) | Server only listens (`TcpListener`) | Add a "dial out" mode to Weylus, **or** run a separate byte-splicing bridge (usbmux `Connect` to the iPad port ↔ `localhost:1701`). The bridge needs no Weylus change; see §5.7. |
| Virtual monitor on KDE | None; only the portal picker | `zkde_screencast stream_virtual_output` or portal `VIRTUAL` plus sizing |
| Input mapped to the virtual output on Wayland | Manual "custom input areas" | Set KWin `InputDevice.outputName` (§3.4) |
| Zero-copy VA-API | CPU copy, `hwupload` | DMA-BUF import (PR #560 is a start) |
| Bitrate control, IDR on demand, raw H.264 | Not available | Protocol and encoder changes |
| Protocol versioning and encryption | None | New handshake; TLS |

**Conclusion:** a native client alone would still leave most of our requirements unmet. Server changes would have to go through a maintainer with very limited bandwidth (one merged PR in 22 months). Our contributions would be BSD-3 inside an AGPL work. The approach is technically feasible but carries a high schedule risk.

### 1.10 What we can reuse
Ideas only (AGPL):
- the PointerEvent-shaped event model;
- the hover-pen auto-removal workaround;
- "fragment per frame" if we ever need a browser client;
- the portal `persist_mode=2` usage;
- the custom input-area concept.

---

## 2. Sunshine and Moonlight

### 2.1 Sunshine overview
- A self-hosted GameStream-compatible host. Clients are Moonlight on iOS, Android and Qt.
- **GPL-3.0**; 42,785 stars. Last commit 2026-10-28. Latest stable release v2026.914.233613 (2026-09-15); pre-releases v2026.1028.* (2026-10-28) ([repo](https://github.com/LizardByte/Sunshine), [releases](https://github.com/LizardByte/Sunshine/releases)).
- **Ports** are offsets from base port 47989:
  - HTTP 47989 (`PORT_HTTP = 0`); HTTPS 47984 (`PORT_HTTPS = -5`) ([src/nvhttp.h](https://github.com/LizardByte/Sunshine/blob/master/src/nvhttp.h));
  - **UDP** video 47998 (+9), control 47999 (+10, ENet), audio 48000 (+11) ([src/stream.h](https://github.com/LizardByte/Sunshine/blob/master/src/stream.h));
  - RTSP setup 48010 (+21) ([src/rtsp.h](https://github.com/LizardByte/Sunshine/blob/master/src/rtsp.h)).
  - Clients pair through a multi-phase handshake over nvhttp ([src/nvhttp.h](https://github.com/LizardByte/Sunshine/blob/master/src/nvhttp.h)).
  - Video uses Reed–Solomon FEC (moonlight-common-c depends on `nanors`; [commit "Bump nanors"](https://github.com/moonlight-stream/moonlight-common-c/commits/master)).
  - **Because the media and control streams are UDP, they cannot be tunnelled through usbmuxd, which only carries TCP streams opened from the host to the device** (§5.7).

### 2.2 The Sunshine pen/touch protocol extension (moonlight-common-c)
Source: [src/Limelight.h L615-682, L1017-1019](https://github.com/moonlight-stream/moonlight-common-c/blob/master/src/Limelight.h) and [src/Input.h L126-155](https://github.com/moonlight-stream/moonlight-common-c/blob/master/src/Input.h). The library is GPL-3.0; last commit 2026-10-26.

```c
int LiSendPenEvent(uint8_t eventType, uint8_t toolType, uint8_t penButtons,
                   float x, float y, float pressureOrDistance,
                   float contactAreaMajor, float contactAreaMinor,
                   uint16_t rotation, uint8_t tilt);
```
- `eventType`: `LI_TOUCH_EVENT_HOVER 0x00`, `DOWN 0x01`, `UP 0x02`, `MOVE 0x03`, `CANCEL 0x04`, `BUTTON_ONLY 0x05`, `HOVER_LEAVE 0x06`, `CANCEL_ALL 0x07`.
- `toolType`: `LI_TOOL_TYPE_UNKNOWN 0`, `PEN 1`, `ERASER 2`.
- `penButtons`: bitmask `PRIMARY 0x01`, `SECONDARY 0x02`, `TERTIARY 0x04`.
- `x`, `y`: normalized 0..1 over the video area.
- `pressureOrDistance`: pressure 0..1 for down and move events. For hover events it is the **distance**, 1.0 = farthest and 0.0 = unknown.
- `contactAreaMajor`/`Minor`: an ellipse in normalized coordinates; 0 means unknown.
- `rotation`: "degrees from vertical in Y dimension (parallel to screen, 0..360)"; `LI_ROT_UNKNOWN 0xFFFF`. This is **azimuth, not barrel roll.**
- `tilt`: "degrees from vertical in Z dimension (perpendicular to screen, 0..90)"; `LI_TILT_UNKNOWN 0xFF`.
- Wire struct `SS_PEN_PACKET` (magic `0x55000003`): header, u8 eventType, u8 toolType, u8 penButtons, 1 reserved byte, netfloat x, y, pressureOrDistance, u16 rotation, u8 tilt, 1 reserved byte, netfloat contactAreaMajor and Minor.
- Host support is advertised by `LI_FF_PEN_TOUCH_EVENTS (0x01)` from `LiGetHostFeatureFlags()`.
- **Gaps for us:** no barrel-roll field; no squeeze or double-tap event (they could be mapped to `penButtons` or keys); no host-to-client pen haptics. The only feedback callbacks are gamepad rumble ([Limelight.h L445-467](https://github.com/moonlight-stream/moonlight-common-c/blob/master/src/Limelight.h)). Adding a field means coordinated changes to moonlight-common-c, Sunshine and every client.

### 2.3 moonlight-ios and Apple Pencil
Source: [Limelight/Input/StreamView.m](https://github.com/moonlight-stream/moonlight-ios/blob/master/Limelight/Input/StreamView.m). GPL-3.0 ([LICENSE.txt](https://github.com/moonlight-stream/moonlight-ios/blob/master/LICENSE.txt)); last commit 2026-09-26; no GitHub releases (it ships on the App Store).

**Yes, it sends Apple Pencil pen events:**
- `UITouchTypePencil` touches go to `LiSendPenEvent(…, LI_TOOL_TYPE_PEN, 0, x, y, (force/maximumPossibleForce)/sin(altitudeAngle), 0, 0, rotation(azimuth), tilt(altitude))` (L243-277).
- Azimuth is converted with a 90° offset ("iOS reports azimuth of 0 when the stylus is pointing west"). Tilt = 90 − altitude (L224-241).
- Hover comes from a `UIHoverGestureRecognizer` restricted to the Pencil (L113-115). It reports `zOffset` as the distance (iOS 16.1+) and azimuth/altitude (iOS 16.4+), then sends `HOVER`/`HOVER_LEAVE` (L279-317).
- It only uses pen events when the host advertises `LI_FF_PEN_TOUCH_EVENTS` (commit "Don't consume stylus events unless they're supported by the host", 2023-12-22).

**Not implemented:**
- coalesced or predicted touches (no `coalescedTouches` in the file);
- eraser, since `toolType` is always PEN and `penButtons` always 0;
- barrel roll (`rollAngle`), squeeze and double-tap (`UIPencilInteraction`);
- haptics.

**Open Pencil issues:**
- [#694](https://github.com/moonlight-stream/moonlight-ios/issues/694): "Initial Apple Pencil activation latency before stroke begins", about 200–300 ms.
- [#638](https://github.com/moonlight-stream/moonlight-ios/issues/638): pressure is not updated during a stroke and lift is not detected.
- [#584](https://github.com/moonlight-stream/moonlight-ios/issues/584): hover fails when a keyboard is connected.

### 2.4 How Sunshine injects pens on Linux
- **Today: libvirtualhid** (MIT). [PR #5368, commit 687e12d](https://github.com/LizardByte/Sunshine/commit/687e12d013a838e1769ffdf996eb1d5ef8402660), dated 2026-08-16, removed `src/platform/linux/input/inputtino_*.cpp` and `third-party/inputtino`.
  - Pen and touch are gated by the `native_pen_touch` config option ([src/platform/linux/input/virtualhid.cpp L28](https://github.com/LizardByte/Sunshine/blob/master/src/platform/linux/input/virtualhid.cpp)).
  - **Polar to Cartesian tilt conversion:** `tilt_x = atan2(sin(-rot)·sin(tilt), cos(tilt))` and `tilt_y = atan2(cos(-rot)·sin(tilt), cos(tilt))`, in degrees. Pressure goes out when touching, distance when hovering. Azimuth is used only for this conversion; it is **not** injected as rotation ([src/platform/virtualhid_input.cpp L997-1075](https://github.com/LizardByte/Sunshine/blob/master/src/platform/virtualhid_input.cpp)).
- **libvirtualhid Linux pen tablet** ([src/platform/linux/uhid_backend.cpp L85-93, L1327-1374, L2169-2330](https://github.com/LizardByte/libvirtualhid/blob/master/src/platform/linux/uhid_backend.cpp)), created with libevdev over uinput:
  - keys: `BTN_TOUCH`, `BTN_STYLUS`, `BTN_STYLUS2`, `BTN_STYLUS3`, `BTN_TOOL_PEN`, `RUBBER`, `BRUSH`, `PENCIL`, `AIRBRUSH`;
  - `ABS_X` 0..19200 and `ABS_Y` 0..10800 (res 28);
  - `ABS_PRESSURE` 0..4096; `ABS_DISTANCE` 0..1024;
  - `ABS_TILT_X` and `ABS_TILT_Y` −90..90 (res 28, written as radians × 28);
  - `INPUT_PROP_POINTER` and `INPUT_PROP_DIRECT`;
  - **no `ABS_Z`, so no barrel roll.**
  - Licensing ([LICENSE.md](https://github.com/LizardByte/libvirtualhid/blob/master/LICENSE.md), [license-map](https://github.com/LizardByte/libvirtualhid/blob/master/LICENSES/license-map.md)): MIT for the library and non-driver backends; LicenseRef-LizardByte-SAL-1.0 for the Windows UMDF driver and macOS broker. Last commit 2026-10-28.
- **inputtino** ([games-on-whales/inputtino](https://github.com/games-on-whales/inputtino); **MIT**, [LICENSE](https://github.com/games-on-whales/inputtino/blob/stable/LICENSE); last commit 2026-07-13; no releases). Its `PenTablet` ([src/uinput/pentablet.cpp L24-69](https://github.com/games-on-whales/inputtino/blob/stable/src/uinput/pentablet.cpp)) has the same layout as libvirtualhid:
  - `MAX_X` 19200, `MAX_Y` 10800, `RESOLUTION` 28;
  - `PRESSURE_MAX` 4096, `DISTANCE_MAX` 1024;
  - tilt −90..90 at res 28 ("units/radian");
  - `BTN_STYLUS`/`STYLUS2`/`STYLUS3`; tools pen, rubber, brush, pencil, airbrush;
  - `INPUT_PROP_POINTER` and `INPUT_PROP_DIRECT`.
  - It has Rust and Python bindings ([bindings/](https://github.com/games-on-whales/inputtino/tree/stable/bindings)).
- **KDE pitfall:** Sunshine issue [#5371](https://github.com/LizardByte/Sunshine/issues/5371) reports that the virtual touchscreen does not work on KDE Wayland until its KWin D-Bus `outputName` is set. KWin logs "Could not find logical output". There is no maintainer response. The same mechanism probably applies to pen tablets, but I did not verify that. A related GNOME multi-monitor pen misalignment issue is [#5592](https://github.com/LizardByte/Sunshine/issues/5592).

### 2.5 Sunshine's KDE Wayland capture
Sunshine has four Linux capture backends ([src/platform/linux/](https://github.com/LizardByte/Sunshine/tree/master/src/platform/linux)):
- **kmsgrab**: temporarily takes `CAP_SYS_ADMIN` to read DRM planes ([kmsgrab.cpp](https://github.com/LizardByte/Sunshine/blob/master/src/platform/linux/kmsgrab.cpp)).
- **wlgrab**: `zwlr_screencopy`, wlroots only ([wayland.cpp](https://github.com/LizardByte/Sunshine/blob/master/src/platform/linux/wayland.cpp)).
- **kwingrab**: "KWin direct ScreenCast capture via zkde_screencast_unstable_v1 … Bypasses xdg-desktop-portal entirely" ([kwingrab.cpp L1-9](https://github.com/LizardByte/Sunshine/blob/master/src/platform/linux/kwingrab.cpp); added by [PR #4724](https://github.com/LizardByte/Sunshine/pull/4724), merged 2026-02).
  - It needs a `.desktop` file with `X-KDE-Wayland-Interfaces=zkde_screencast_unstable_v1` whose `Exec` names the executable, or the environment variable `KWIN_WAYLAND_NO_PERMISSION_CHECKS=1` (L44-60).
  - It only calls `stream_output` on existing outputs, with the pointer embedded (L425). **It does not create virtual outputs.**
- **portalgrab**: xdg portal ScreenCast/RemoteDesktop with restore tokens; `types = SOURCE_TYPE_MONITOR` only ([portalgrab.cpp L19, L609-663](https://github.com/LizardByte/Sunshine/blob/master/src/platform/linux/portalgrab.cpp); PRs [#5774](https://github.com/LizardByte/Sunshine/pull/5774), [#5856](https://github.com/LizardByte/Sunshine/pull/5856)).

Open issue [#6009](https://github.com/LizardByte/Sunshine/issues/6009) (2026-10-28): **KWin plus VA-API gives a black screen on an AMD RX 6700 XT** (`EGL_BAD_PARAMETER`), and the memory-buffer workaround adds latency. This is directly relevant to our AMD/VA-API target.

### 2.6 Virtual displays on Linux
- "Virtual displays not supported on linux" [#5266](https://github.com/LizardByte/Sunshine/issues/5266) was **closed as not planned** (2026-06-07). The maintainer: "We don't currently support virtual displays natively on any platform … it should work with virtual displays that you create".
- Community workarounds:
  - [Sunveil (ImStillBlue/sunshine-virtual-display)](https://github.com/ImStillBlue/sunshine-virtual-display) (MIT): runs `krfb-virtualmonitor --resolution WxH` and `kscreen-doctor` custom modes from Sunshine's `global_prep_cmd` hooks on KDE Plasma 6.
  - [diegostrg/ipad-virtual-monitor](https://github.com/diegostrg/ipad-virtual-monitor) (MIT; Hyprland; Sunshine plus Moonlight over **USB-C NCM networking**, §5.7). It reports "2360x1640 at 60 fps, HEVC, … 2 ms of network latency".
  - [ACKB/ipad-second-screen](https://github.com/ACKB/ipad-second-screen) (MIT; Sunshine plus Moonlight on Wayland).
- Forks: [Apollo](https://github.com/ClassicOldSong/Apollo) (GPL-3.0) does virtual displays on **Windows only** through SudoVDA ([Sunveil README](https://github.com/ImStillBlue/sunshine-virtual-display)). [Hermes](https://github.com/MrOz59/Hermes) (GPL-3.0, Apollo-derived, Linux, last push 2026-10-28) offers EVDI or a custom "Hermes-KMS" DRM virtual display that feeds the scanout DMA-BUF to the encoder; it is developed on KDE/AMD.

### 2.7 VA-API settings in Sunshine
- From [src/video.cpp L1234-1293](https://github.com/LizardByte/Sunshine/blob/master/src/video.cpp):
  - `h264_vaapi`, `hevc_vaapi` and `av1_vaapi` with `async_depth=1`, `blbrc` (configurable), `idr_interval=INT_MAX`, `sei=0`;
  - flags `LIMITED_GOP_SIZE | PARALLEL_ENCODING | NO_RC_BUF_LIMIT`.
- General low-delay settings ([video.cpp L2054-2085](https://github.com/LizardByte/Sunshine/blob/master/src/video.cpp)):
  - `max_b_frames=0` ("B-frames delay decoder output");
  - **infinite GOP** with IDR on demand;
  - client-limited `refs` (enables reference-frame invalidation);
  - `AV_CODEC_FLAG_CLOSED_GOP | AV_CODEC_FLAG_LOW_DELAY`, `FLAG2_FAST`.
- A Vulkan Video encoder also exists ([vulkan_encode.cpp](https://github.com/LizardByte/Sunshine/blob/master/src/platform/linux/vulkan_encode.cpp)).

### 2.8 moonlight-qt
- GPL-3.0; v6.2.0 (2026-10-04); last commit 2026-10-29 ([repo](https://github.com/moonlight-stream/moonlight-qt)).
- It sends pen events from desktop clients ([app/streaming/input/abstouch.cpp](https://github.com/moonlight-stream/moonlight-qt/blob/master/app/streaming/input/abstouch.cpp)). Not relevant as an iPad client.

### 2.9 What we can reuse from Sunshine and Moonlight
- **Code:** none from Sunshine or Moonlight (GPL-3.0). **The MIT pieces they depend on are reusable:** libvirtualhid's Linux backend and inputtino.
- **Ideas:**
  - polar-to-Cartesian tilt maths and the iOS angle conventions;
  - IDR on demand with an infinite GOP, `async_depth=1`, no B-frames;
  - FEC over UDP for Wi-Fi;
  - the KWin `.desktop` permission trick;
  - feature-flag negotiation (`LI_FF_*`).
- **Interop:** implementing a Moonlight-compatible server or client from the protocol is legally fine. It does not fit our needs, though: UDP transport, game-launcher semantics, and no barrel roll or haptics.
- **As a baseline:** today the user can already run Sunshine (KWin capture) + krfb-virtualmonitor/Sunveil + Moonlight iOS over Wi-Fi, or over USB-NCM with a git build of usbmuxd. That gives a virtual monitor plus pen pressure, tilt and hover. Use it as the benchmark for our latency.

---

## 3. KRdp and KPipeWire (plus the KWin and portal plumbing they use)

### 3.1 KRdp (invent.kde.org/plasma/krdp)
- **What:** "A Remote Desktop Protocol server for the KDE Plasma desktop", built on FreeRDP 3.1+ (server side), KPipeWire and libei ([README](https://invent.kde.org/plasma/krdp/-/blob/master/README.md); [CMakeLists.txt](https://invent.kde.org/plasma/krdp/-/blob/master/CMakeLists.txt)).
- **Status:** last commit 2026-10-29; tags v6.8.2 (2026-10-27) and v6.8.1. It ships with Plasma (Arch `extra/krdp` 6.8.2-1, [archlinux.org](https://archlinux.org/packages/extra/x86_64/krdp/)).
- **Virtual monitors:** the README offers "allow only one session at a time (with the remote user seeing a virtual display)".
  - `PlasmaScreencastV1Session::start()` calls `createVirtualMonitorStream(name, size, dpr, Metadata)` and records the mapping id `"Virtual-" + name`, with the comment "KWin prefixes virtual output names with 'Virtual-'" ([src/PlasmaScreencastV1Session.cpp L74-80](https://invent.kde.org/plasma/krdp/-/blob/master/src/PlasmaScreencastV1Session.cpp)).
  - That function calls `zkde_screencast_unstable_v1.stream_virtual_output(name, w, h, scale, mode)` ([src/screencasting.cpp L159-162](https://invent.kde.org/plasma/krdp/-/blob/master/src/screencasting.cpp)).
  - The size comes from the RDP Display Control channel monitor layout, single monitor only ([src/DisplayControl.cpp](https://invent.kde.org/plasma/krdp/-/blob/master/src/DisplayControl.cpp)).
  - A `PortalSession` path also exists (xdg ScreenCast/RemoteDesktop).
- **Input:** D-Bus `org.kde.KWin /org/kde/KWin/EIS/RemoteDesktop connectToEIS(7)`, where 7 = Keyboard | Pointer | TouchScreen ([PlasmaScreencastV1Session.cpp L37-58](https://invent.kde.org/plasma/krdp/-/blob/master/src/PlasmaScreencastV1Session.cpp)).
  - The libei client maps absolute pointer events into the libei region whose `mapping_id` matches the streamed output ([src/EiConnection.cpp](https://invent.kde.org/plasma/krdp/-/blob/master/src/EiConnection.cpp)).
  - It was ported to libei in Oct 2026 (MRs [!259](https://invent.kde.org/plasma/krdp/-/merge_requests/259) and [!278](https://invent.kde.org/plasma/krdp/-/merge_requests/278)).
  - **No pen or tablet input** in KRdp; I found no RDPEI or pen MRs.
- **Video:** `PipeWireEncodedStream` with `EncodingPreference::Speed`, `H264Baseline`, full colour range, a quality setting and `setMaxPendingFrames` ([src/VideoStreamSurface.cpp L76-83](https://invent.kde.org/plasma/krdp/-/blob/master/src/VideoStreamSurface.cpp)). Output goes over the RDPGFX AVC420 path with adaptive quality ([src/VideoStream.cpp](https://invent.kde.org/plasma/krdp/-/blob/master/src/VideoStream.cpp)).
- **License:** the SPDX headers are `LGPL-2.1-only OR LGPL-3.0-only OR LicenseRef-KDE-Accepted-LGPL` for most files (e.g. `src/PlasmaScreencastV1Session.cpp`, `src/EiConnection.cpp`, `server/main.cpp`). **`src/VideoStream.cpp` is `GPL-2.0-or-later`.** GitLab's license detector reports "other".

### 3.2 KPipeWire (invent.kde.org/plasma/kpipewire)
- **What:** "convenient classes to use PipeWire in Qt projects". `KPipeWire` renders streams; `KPipeWireRecord` records and encodes through FFmpeg ([README](https://invent.kde.org/plasma/kpipewire/-/blob/master/README.md)).
- It exposes a `PipeWireEncodedStream` that emits encoded packets, which KRdp uses.
- Encoders: `h264vaapiencoder`, `libx264encoder`, `libopenh264encoder`, `libvpx(vp9)encoder`, `libwebp`, `gif` ([src/](https://invent.kde.org/plasma/kpipewire/-/tree/master/src)).
- **The VA-API path uses libav (FFmpeg), not GStreamer** ([src/h264vaapiencoder.cpp](https://invent.kde.org/plasma/kpipewire/-/blob/master/src/h264vaapiencoder.cpp)):
  - The input is an `AV_PIX_FMT_DRM_PRIME` buffer source (a PipeWire DMA-BUF), passed through the filter graph `hwmap=mode=direct:derive_device=vaapi,scale_vaapi=format=nv12:mode=fast:out_range=…` (L60, L101). That makes it **zero-copy**.
  - Encoder `h264_vaapi` with `max_b_frames=0`, `gop_size=100`, `global_quality` mapped from a percentage, and profile Baseline (constrained), Main or High (L119-155).
  - In Speed mode, `async_depth=1` ("a depth of >1 can leave a pending frame queued indefinitely"); also `flags=+mv4` and `-flags=+loop` (L176-190).
- **Status:** last commit 2026-10-25 ("PipeWireSourceStream: Size the format params buffer to fit"); tag v6.8.2 (2026-10-27).
- **License:** README says "LGPL v2.1+"; SPDX headers say `LGPL-2.1-only OR LGPL-3.0-only OR LicenseRef-KDE-Accepted-LGPL`. **`src/dmabufhandler.cpp` is `Apache-2.0`.**

### 3.3 The KWin and portal plumbing (verified)
- **Protocol** [`zkde_screencast_unstable_v1` v6](https://github.com/KDE/plasma-wayland-protocols/blob/master/src/protocols/zkde-screencast-unstable-v1.xml) (LGPL-2.1-or-later XML):
  - requests: `stream_output`, `stream_window`, `stream_virtual_output(name, width, height, scale, pointer)` (since v2), `stream_region` (v3), `stream_virtual_output_with_description` (v4);
  - events: `created(node)` (deprecated in v6), `failed`, `closed`, `serial` (v6).
  - It is marked "a desktop environment implementation detail. Regular clients must not use this protocol."
  - **A refresh-rate parameter is not part of the request.**
- **KWin implementation:**
  - `ScreencastManager::streamVirtualOutput` creates `outputBackend()->createVirtualOutput(name, description, size, scale)`, streams it, and **removes the output when the stream finishes** ([src/plugins/screencast/screencastmanager.cpp L73-90](https://github.com/KDE/kwin/blob/master/src/plugins/screencast/screencastmanager.cpp)). The output's lifetime equals the stream's lifetime.
  - In the DRM backend it is a `DrmVirtualOutput` with capability `CustomModes` ([drm_backend.cpp L334-341](https://github.com/KDE/kwin/blob/master/src/backends/drm/drm_backend.cpp)). Its default mode is `size @ 60000` mHz ([drm_virtual_output.cpp L29](https://github.com/KDE/kwin/blob/master/src/backends/drm/drm_virtual_output.cpp)).
  - Higher refresh rates (for example 120 Hz ProMotion) therefore need a custom mode, set the way Sunveil does with `kscreen-doctor`.
- **Rust bindings:** the MIT crate [`wayland-protocols-plasma`](https://github.com/Smithay/wayland-rs/blob/master/wayland-protocols-plasma/src/lib.rs) has a `screencast::v1` module generated from that XML (crate 0.3.12, [crates.io](https://crates.io/crates/wayland-protocols-plasma)).
- **Portal alternative:** [xdg-desktop-portal-kde `screencast.cpp`](https://invent.kde.org/plasma/xdg-desktop-portal-kde/-/blob/master/src/screencast.cpp) supports `Output::Virtual` and calls `WaylandIntegration::startStreamingVirtual(...)` (L233-238).
  - The user picks the size in the dialog; the default is 1920×1080 (L345).
  - The size is saved in the restore-token payload as `virtualSize` (L297-298, L375-377).
  - So the portal path works without special permissions but **cannot set the size from code**. The direct `zkde_screencast` path can, but needs the `.desktop` `X-KDE-Wayland-Interfaces` entitlement (§2.5).
- **krfb-virtualmonitor:** [krfb/main-virtualmonitor.cpp](https://invent.kde.org/network/krfb/-/blob/master/krfb/main-virtualmonitor.cpp) (GPL-3.0-or-later) is a VNC server that creates a virtual monitor with `--resolution`, `--scale` and `--port`, Wayland only.
- **Pinning input to an output:** KWin exports each libinput device on D-Bus as `org.kde.KWin.InputDevice` at `/org/kde/KWin/InputDevice/<sysname>` ([device.h L60](https://github.com/KDE/kwin/blob/master/src/backends/libinput/device.h), [device.cpp L459](https://github.com/KDE/kwin/blob/master/src/backends/libinput/device.cpp)). Relevant properties:
  - `tabletTool`;
  - writable `outputName` (L74);
  - `outputArea` and `mapToWorkspace` (L172-176);
  - `pressureCurve` and `pressureRangeMin`/`Max` (L179-191);
  - `tabletToolIsRelative`.
  - Setting `outputName="Virtual-<name>"` on our uinput pen pins it to the virtual monitor. Compare Sunshine #5371 (§2.4).

### 3.4 Reuse potential from KRdp and KPipeWire
- **Code:** LGPL can be **dynamically linked** from MIT or Apache code (e.g. link `libKPipeWireRecord.so` for `PipeWireEncodedStream`). However, it brings in Qt 6, KF6 and C++ ABI coupling, which is awkward from a Rust host. `dmabufhandler.cpp` (Apache-2.0) can be copied. Do not copy `VideoStream.cpp` (GPL).
- **Ideas (the most valuable part):**
  - virtual monitor via `stream_virtual_output`, with the output torn down when the stream ends;
  - the `"Virtual-"` naming convention for mapping input;
  - EIS through `org.kde.KWin.EIS.RemoteDesktop.connectToEIS` for keyboard, pointer and touch;
  - the zero-copy VA-API chain (`DRM_PRIME` → `hwmap` → `scale_vaapi` → `h264_vaapi`, `async_depth=1`, no B-frames);
  - Display Control-driven resizing.

---

## 4. Tether (github.com/zackb/tether), brief

- **What:** "Linux + iPhone Continuity / iMessage / SMS".
  - Features: clipboard sync, file transfer, messages, notifications, calls (beta), AirPods (beta) and Continuity Camera (alpha).
  - Pieces: a C++ daemon `tetherd`, a GTK app, a TUI, a CLI, browser and mail extensions, and an iOS app on the App Store.
  - Sources: [README](https://github.com/zackb/tether/blob/main/README.md), [apple/](https://github.com/zackb/tether/tree/main/apple).
- **Platforms:** the Linux side requires Wayland with `wlr-data-control` for clipboard sync. The phone side is the iPhone app (README). iPad is not a stated target. Issue [#262](https://github.com/zackb/tether/issues/262) asks for "Universal Control-style iPad mouse and keyboard sharing" and explicitly says it is "not screen mirroring".
- **Transport:** "WiFi: mTLS :5134, mDNS". Both sides present self-signed X.509 certificates, pinned. Bluetooth (BlueZ, D-Bus) carries messages and notifications (README architecture diagram).
- **Status:** MIT ([LICENSE](https://github.com/zackb/tether/blob/main/LICENSE)). 1,499 stars. Created 2026-04-12. Last commit and release **v0.3.0 on 2026-10-29**, with near-daily releases ([releases](https://github.com/zackb/tether/releases)).
- **How it relates to us:**
  - Our project must coexist with it: avoid port 5134 and use a distinct mDNS service type.
  - Its MIT code, especially the mTLS pinning, pairing UX and Network.framework usage, may be reused. Another agent is studying its security design.
  - Its Continuity Camera feature streams video from iPhone to PC, the opposite direction to ours. That may be a reference for VideoToolbox use.

---

## 5. Other relevant projects

### 5.1 Closest new prior art: iPad plus Apple Pencil to Linux
- **InkLink** ([RakeRaven/inklink](https://github.com/RakeRaven/inklink)): **MIT** ([LICENSE](https://github.com/RakeRaven/inklink/blob/main/LICENSE), [host/Cargo.toml](https://github.com/RakeRaven/inklink/blob/main/host/Cargo.toml)). **Created 2026-10-28, 17 commits all that day, 0 stars, no releases.** "Use an iPad + Apple Pencil as a pen display for a Windows or Linux computer."
  - **Architecture:** the iPad app is the **server**: an `NWListener` on TCP **47800**, advertised over Bonjour as `_inklink._tcp` ([ios/Sources/HostLink.swift](https://github.com/RakeRaven/inklink/blob/main/ios/Sources/HostLink.swift), [host/src/protocol.rs](https://github.com/RakeRaven/inklink/blob/main/host/src/protocol.rs)). The Rust host connects to it.
    - Over **USB** it talks to usbmuxd directly through `/var/run/usbmuxd`, sending plist `ListDevices` and then `Connect` with `PortNumber` byte-swapped to network order ([host/src/net.rs L90-181](https://github.com/RakeRaven/inklink/blob/main/host/src/net.rs)).
    - Over Wi-Fi it uses Bonjour (`mdns-sd`). It tries USB first.
  - **Protocol:** `[u8 kind][u32 LE length][payload]`. Messages: HOST_HELLO, VIDEO, NOTICE from host to iPad; CLIENT_HELLO, STREAM_CONFIG (monitor, fps, kbps, max w/h, mode Fit/Fill/Virtual), KEYFRAME, PAN, PEN, TOUCH, KEY from iPad to host. Version 2.
    - A `PenEvent` carries phase (Hover/Down/Move/Up/Leave), eraser, normalized x and y, pressure, `tilt_x`/`tilt_y` in degrees, and **twist (barrel roll, 0..360)** ([protocol.rs](https://github.com/RakeRaven/inklink/blob/main/host/src/protocol.rs)).
  - **iPad side:**
    - `coalescedTouches` ("full 240 Hz Pencil stream");
    - `rollAngle` (iOS 17.5);
    - `UIPencilInteraction` double-tap and **squeeze**, mapped to eraser, toolbar or shortcuts ([ios/Sources/StreamView.swift L65, L105, L150-171](https://github.com/RakeRaven/inklink/blob/main/ios/Sources/StreamView.swift)).
    - No hover distance (`zOffset`) is sent and there are no haptics (grep found neither).
  - **Linux injection:** `evdev` crate over uinput, devices "InkLink Pen", "InkLink Touch" and a keyboard. The pen has:
    - `BTN_TOOL_PEN`, `BTN_TOOL_RUBBER`, `BTN_TOUCH`, `BTN_STYLUS`;
    - `ABS_X`/`ABS_Y` (res 100); `ABS_PRESSURE` 0..4095;
    - `ABS_TILT_X`/`Y` −90..90 at res 57 (about 1 unit per degree, since 57 ≈ 180/π);
    - **`ABS_Z` −900..899 for barrel roll**;
    - no `ABS_DISTANCE` ([host/src/platform/linux.rs L180-245](https://github.com/RakeRaven/inklink/blob/main/host/src/platform/linux.rs)).
  - **Capture:** `ashpd` portal ScreenCast; **"Extra display" uses `SourceType::Virtual`**, so on KDE the compositor picks the size ([linux.rs L133-170](https://github.com/RakeRaven/inklink/blob/main/host/src/platform/linux.rs)). Then GStreamer `pipewiresrc`, encoders tried in order `vah264lpenc`, `vah264enc`, `vaapih264enc`, `x264enc` with zerolatency or low-latency properties, into an `appsink` ([host/src/capture.rs](https://github.com/RakeRaven/inklink/blob/main/host/src/capture.rs)).
  - **Caveats:**
    - one day old and single author;
    - not tested at scale;
    - the repo contains a **vendored Windows virtual-display driver binary** (`VirtualDisplayDriver/MttVDD.dll`) and stray Windows cache `.db` files under `host/%SystemDrive%/`;
    - input mapping on KDE is left to the user ("map them … in your compositor's tablet settings").
  - It is still the **best MIT-licensed starting point or collaboration target**.
- **InkWand** ([LorenzoFerri/InkWand](https://github.com/LorenzoFerri/InkWand)): Swift on both sides. iPad app plus a Linux/macOS server that exposes uinput pen, touch and pad devices. USB goes through `iproxy`. **No LICENSE file, so all rights reserved.** Last push 2026-07-12. Ideas only.
- **Others (tiny, recent, varied licenses):**
  - [noturbob/Penblem](https://github.com/noturbob/Penblem) (Rust; no license);
  - [Davidio1777/ipad-tablet-backends](https://github.com/Davidio1777/ipad-tablet-backends) (MIT; OpenTabletDriver-compatible);
  - [rsmagulov/ipad-linux-monitor](https://github.com/rsmagulov/ipad-linux-monitor) (MIT; iOS 12 app; **X11 only**; usbmuxd plus TCP 27183 with TLS-PSK; NVENC/VA-API/x264; "spare HDMI/DP port forced connected" as the virtual monitor);
  - [arjo129/remote_tablet](https://github.com/arjo129/remote_tablet) (2020);
  - [Marukiee/Tandem](https://github.com/Marukiee/Tandem) (AGPL-3.0; Rust; "Apple ecosystem on every device"; Linux "experimental").

### 5.2 Older tablet-to-PC tools
- **GfxTablet** ([rfc2822/GfxTablet](https://github.com/rfc2822/GfxTablet)): **archived**, "This project is not maintained". **MIT** ([LICENSE.md](https://github.com/rfc2822/GfxTablet/blob/master/LICENSE.md)).
  - Android app; UDP port **40118**; a packed `event_packet` {signature[9], u16 version=2, u8 type (motion/button), u16 x, y, pressure, i8 button, i8 down} ([driver-uinput/protocol.h](https://github.com/rfc2822/GfxTablet/blob/master/driver-uinput/protocol.h)).
  - Its uinput tablet has `BTN_TOOL_PEN`, `BTN_STYLUS`/`2`, `ABS_X`/`Y`/`PRESSURE` and no tilt ([networktablet.c](https://github.com/rfc2822/GfxTablet/blob/master/driver-uinput/networktablet.c)). Historical interest only.
- **VirtualTablet** (Sunnysidesoft, proprietary):
  - The Wi-Fi/USB app needs a Windows or macOS server; its listing says "Linux is not supported yet" ([App Store](https://apps.apple.com/us/app/virtualtablet/id6783119709)).
  - A separate **"VirtualTablet: Bluetooth"** app claims "no extra software needed" on **Linux**, with Pencil pressure, tilt, double-tap and hover. It appears to present itself as a Bluetooth HID device ("VT-HID" in developer replies) ([App Store](https://apps.apple.com/us/app/-/id6758901525)). This is an interesting alternative transport; I have not verified the mechanism.
- **OpenTabletDriver** ([OpenTabletDriver/OpenTabletDriver](https://github.com/OpenTabletDriver/OpenTabletDriver), LGPL-3.0, active): its Linux virtual tablet ([EvdevVirtualTablet.cs](https://github.com/OpenTabletDriver/OpenTabletDriver/blob/master/OpenTabletDriver.Desktop/Interop/Input/Absolute/EvdevVirtualTablet.cs)) uses:
  - `ABS_X`/`Y` at 1000 subpixels per pixel;
  - pressure 0..65535;
  - tilt −64..63 at res 57;
  - **`ABS_Z` 0..65535 for rotation**;
  - `BTN_STYLUS`/`2`/`3`, `INPUT_PROP_DIRECT` and `INPUT_PROP_POINTER`.
  - This confirms `ABS_Z` as the convention for rotation.

### 5.3 Commercial "tablet as second screen" tools: Linux support
- **spacedesk:** "Driver available for: Windows 11/10, Mac OS". Viewers on Android, iOS, Windows and HTML5. **No Linux host** ([spacedesk.net](https://www.spacedesk.net/)).
- **Duet Display:** documented hosts are Windows 10+ and macOS 10.13+. **No Linux** ([Duet help center](https://www.duetdisplay.com/help-center/what-hardware-and-software-does-duet-support)).
- **Astropad Studio:** Mac and Windows only. Linux is explicitly unsupported ([Astropad support](https://support.astropad.com/en/articles/11835972-what-are-the-system-requirements-for-astropad-studio)).
- **SuperDisplay:** an Android app with a Windows driver. No iPad and no Linux ([superdisplay.app](https://superdisplay.app)).
- I did not research Splashtop Wired XDisplay or Luna Display in depth.

### 5.4 Remote-desktop and second-screen open source
- **Deskreen** ([pavlobu/deskreen](https://github.com/pavlobu/deskreen)): **AGPL-3.0**. Electron plus WebRTC to a browser; view only. v3.2.16 (2026-07-08). No pen input. Ideas only.
- **VirtScreen** ([kbumsik/virtscreen](https://github.com/kbumsik/virtscreen)): GPL-3.0; X11 (xrandr plus VNC); last push 2022. Obsolete for Wayland.
- **RustDesk** ([rustdesk/rustdesk](https://github.com/rustdesk/rustdesk)): **AGPL-3.0**; very active.
  - "Stylus support" ([PR #12196](https://github.com/rustdesk/rustdesk/pull/12196), merged 2025-07-01) is Flutter client-side pointer handling. I found no pressure or tilt injection.
  - Linux virtual or headless display is still an open PR ([#15908](https://github.com/rustdesk/rustdesk/pull/15908)).
- **wayvnc** ([any1/wayvnc](https://github.com/any1/wayvnc)): ISC; "A VNC server for wlroots based compositors". **Not KWin.**
- **krfb / krfb-virtualmonitor**: GPL. Creates KWin virtual monitors (§3.3).
- **GNOME Remote Desktop** ([gitlab.gnome.org](https://gitlab.gnome.org/GNOME/gnome-remote-desktop)): GPL-2.0 ([COPYING](https://gitlab.gnome.org/GNOME/gnome-remote-desktop/-/blob/main/COPYING)).
  - RDP and VNC; headless and virtual monitors through Mutter; VA-API and Vulkan encode.
  - It handles RDPEI touch, but pen is a stub: "Missing libei-API to submit (multi-)pen events" ([src/grd-rdp-dvc-input.c L58-59](https://gitlab.gnome.org/GNOME/gnome-remote-desktop/-/blob/main/src/grd-rdp-dvc-input.c)).
  - GNOME only.
- **evdi** ([DisplayLink/evdi](https://github.com/DisplayLink/evdi)): a kernel module (GPL-2.0, [module/LICENSE](https://github.com/DisplayLink/evdi/blob/main/module/LICENSE)) plus a library (LGPL-2.1, [library/LICENSE](https://github.com/DisplayLink/evdi/blob/main/library/LICENSE)); top-level LICENSE is MIT. v1.15.1 (2026-09-15).
  - It creates a real DRM connector, but readback goes through a CPU copy (Hermes README quotes ~180 µs/frame vs ~8 µs for its DMA-BUF driver).
  - There are open KWin issues (e.g. [#388](https://github.com/DisplayLink/evdi/issues/388)).
  - On KDE, `stream_virtual_output` is simpler and fully supported.
- **Barrier / Input Leap / lan-mouse:** keyboard and mouse sharing; no pen or display. **Not relevant.**
  - [input-leap/input-leap](https://github.com/input-leap/input-leap) is archived (`archived: true`, last push 2025-12-01).
  - [debauchee/barrier](https://github.com/debauchee/barrier): last push 2024-06-22.
  - [feschber/lan-mouse](https://github.com/feschber/lan-mouse): GPL-3.0, active.

### 5.5 Linux input stack: verified facts that matter for the pen
- **libinput** ([src/evdev-tablet.c](https://gitlab.freedesktop.org/libinput/libinput/-/blob/main/src/evdev-tablet.c)):
  - It **rejects** a tablet unless it has `ABS_X`/`ABS_Y`, has `BTN_TOOL_PEN` or `BTN_STYLUS`, and has a **resolution**, which it uses for size (L2840-2863).
  - **Tilt:** a non-zero resolution means units per radian. It requires min < 0 < max (L398-410).
  - **Rotation:** the axis is only copied for pen-type tools if `ABS_Z` exists (L1087-1097). It is interpreted Art-Pen style, "artpen has 0 with buttons pointing east" (+90°) (L664-678).
  - `ABS_DISTANCE` is copied when present (L1082).
  - **Smoothing is disabled by default for uinput (virtual) devices** (L2819-2835). Good for latency.
- **libei** ([src/libei.h](https://gitlab.freedesktop.org/libinput/libei/-/blob/main/src/libei.h)):
  - `EI_DEVICE_CAP_STYLUS` and `ei_stylus_capability` {ERASE, PRESSURE, DISTANCE, TILT, ROTATION, AIRBRUSH_FLOW}, plus `ei_device_stylus_*()` calls for proximity, tip, motion, pressure, distance, tilt and rotation. These are marked **"@since 1.7"**. MR [!400](https://gitlab.freedesktop.org/libinput/libei/-/merge_requests/400) was merged 2026-07-29.
  - **The latest tag is 1.6.0 (2026-05-15)**, which does not contain `EI_DEVICE_CAP_STYLUS` ([1.6.0 libei.h](https://gitlab.freedesktop.org/libinput/libei/-/blob/1.6.0/src/libei.h)). Arch ships libei 1.6.0 ([archlinux.org](https://archlinux.org/packages/extra/x86_64/libei/)).
  - Searching KWin MRs for EIS stylus or tablet support found none ([invent.kde.org/plasma/kwin MRs](https://invent.kde.org/plasma/kwin/-/merge_requests)). KRdp requests only Keyboard | Pointer | TouchScreen.
  - So **uinput is the only path today**. libei stylus is a future option that would not need `/dev/uinput` permissions; the Rust crate is [`reis`](https://crates.io/crates/reis), MIT.

### 5.6 Apple Pencil APIs relevant to the gaps (Apple developer docs)
- [`UITouch.rollAngle`](https://developer.apple.com/documentation/uikit/uitouch/rollangle) and [`UIHoverGestureRecognizer.rollAngle`](https://developer.apple.com/documentation/uikit/uihovergesturerecognizer/rollangle): barrel roll, iOS/iPadOS 17.5.
- [`UIPencilInteraction.Squeeze`](https://developer.apple.com/documentation/uikit/uipencilinteraction/squeeze) and [`UIPencilInteraction.Tap`](https://developer.apple.com/documentation/uikit/uipencilinteraction/tap): iPadOS 17.5.
- [`UIHoverGestureRecognizer.zOffset`](https://developer.apple.com/documentation/uikit/uihovergesturerecognizer/zoffset): normalized hover distance, iPadOS 16.1.
- [`UICanvasFeedbackGenerator`](https://developer.apple.com/documentation/uikit/uicanvasfeedbackgenerator): canvas haptics (Pencil Pro), iPadOS 17.5. The app must trigger it, so haptics driven by the PC need a **host-to-iPad message**. None of the surveyed projects has one.
- [`UIEvent.coalescedTouches(for:)`](https://developer.apple.com/documentation/uikit/uievent/coalescedtouches(for:)) and [`predictedTouches(for:)`](https://developer.apple.com/documentation/uikit/uievent/predictedtouches(for:)): iOS 9.

### 5.7 USB transport: usbmuxd, libimobiledevice, idevice, PeerTalk
- **The usbmuxd model:** host programs talk to the usbmuxd daemon over `/var/run/usbmuxd` (TCP 27015 on Windows). The header is {u32 length, u32 version, u32 message, u32 tag}. Message types are RESULT 1, CONNECT 2, LISTEN 3, DEVICE_ADD 4, DEVICE_REMOVE 5, PAIRED 6 and PLIST 8. A connect request carries {device_id, u16 port, reserved} ([libusbmuxd include/usbmuxd-proto.h](https://github.com/libimobiledevice/libusbmuxd/blob/master/include/usbmuxd-proto.h)).
  - **The host opens TCP connections to ports the iOS app listens on.** The iPad must therefore be the TCP server, as in InkLink (§5.1) and PeerTalk.
  - The protocol is simple enough to implement directly. InkLink does it in about 90 lines with the MIT `plist` crate.
- **usbmuxd** ([libimobiledevice/usbmuxd](https://github.com/libimobiledevice/usbmuxd)): GPL-2.0 per the API (the Arch PKGBUILD says `GPL2 GPL3`). Last commit 2025-12-06. **Last release 1.1.1 (2020-06-15).**
  - We only talk to the daemon over its socket, so its license does not affect our code.
  - **USB networking (CDC-NCM):** git `src/usb.c` can switch Apple devices into mode 3 ("CDC NCM", Ethernet over USB), or modes 4 and 5 ("usbeth+cdc_ncm", "cdc_ncm direct"), via the environment variable `USBMUXD_DEFAULT_DEVICE_MODE` ([usb.c L397-461, L706-729, L788-791](https://github.com/libimobiledevice/usbmuxd/blob/master/src/usb.c); [usb.h L53](https://github.com/libimobiledevice/usbmuxd/blob/master/src/usb.h)). These commits date from 2023-09 to 2024-09 (["usb: add support for modes 4 and 5"](https://github.com/libimobiledevice/usbmuxd/commits/master/src/usb.c)).
  - **Arch's `usbmuxd 1.1.1-4` builds the 2020 tag** ([PKGBUILD `_commit=…# tags/1.1.1`](https://gitlab.archlinux.org/archlinux/packaging/packages/usbmuxd/-/blob/main/PKGBUILD)), so it lacks NCM mode switching.
  - [diegostrg/ipad-virtual-monitor docs/usb-network.md](https://github.com/diegostrg/ipad-virtual-monitor/blob/main/docs/usb-network.md) documents running Sunshine and Moonlight over this NCM link. NCM would let UDP protocols work over the cable, but it needs root-level setup: a git build of usbmuxd, NetworkManager sharing and a DHCP server.
- **libusbmuxd** (LGPL-2.1, last commit 2025-09-07) includes `iproxy` (GPL-2.0-or-later, [tools/iproxy.c](https://github.com/libimobiledevice/libusbmuxd/blob/master/tools/iproxy.c)), which forwards a local port to a device port. **libimobiledevice** (LGPL-2.1, last commit 2026-06-10).
- **idevice** ([jkcoxson/idevice](https://github.com/jkcoxson/idevice)): **MIT**, Rust, v0.1.68 (2026-09-15). Feature `usbmuxd` ("Connect using the usbmuxd daemon"), plus lockdown, RSD/XPC, CoreDevice tunnels and a tunneld client ([README](https://github.com/jkcoxson/idevice/blob/master/README.md)). The README says the API "is far from final".
- **go-ios** ([danielpaulus/go-ios](https://github.com/danielpaulus/go-ios), MIT, active) and **pymobiledevice3** ([doronz88/pymobiledevice3](https://github.com/doronz88/pymobiledevice3), GPL-3.0, active) are references for the iOS 17+ RemoteXPC/CoreDevice details.
- **PeerTalk** ([rsms/peertalk](https://github.com/rsms/peertalk)): **MIT**. Last code 2020; last push 2022-04-27.
  - An iOS/macOS Cocoa library. The iOS side listens on a TCP port; the Mac side connects through usbmuxd ("Can connect to TCP services on supported attached devices … bridging the communication over USB transport").
  - Frame = {u32 version, u32 type, u32 tag, u32 payloadSize}, big-endian ([PTProtocol.m](https://github.com/rsms/peertalk/blob/master/peertalk/PTProtocol.m)).
  - Its README cites Duet Display as a user. On modern iOS a plain `NWListener` replaces it (InkLink, Tether).
- **Rust crates for the host** (licenses from crates.io, 2026-10-30):
  - [`ashpd`](https://crates.io/crates/ashpd) MIT (portals);
  - [`pipewire`](https://crates.io/crates/pipewire) MIT;
  - [`gstreamer`](https://crates.io/crates/gstreamer) MIT OR Apache-2.0;
  - [`evdev`](https://crates.io/crates/evdev) Apache-2.0 OR MIT;
  - [`wayland-protocols-plasma`](https://crates.io/crates/wayland-protocols-plasma) MIT;
  - [`reis`](https://crates.io/crates/reis) MIT (libei);
  - [`mdns-sd`](https://crates.io/crates/mdns-sd) Apache-2.0 OR MIT;
  - [`plist`](https://crates.io/crates/plist) MIT;
  - [`idevice`](https://crates.io/crates/idevice) MIT;
  - [`ffmpeg-next`](https://crates.io/crates/ffmpeg-next) WTFPL. FFmpeg itself is LGPL or GPL depending on build, so linking needs care.

---

## 6. License compatibility (MIT or Apache-2.0 project)

| Source | License | May we copy code into MIT/Apache? | May we link it? | Protocol interop? |
|---|---|---|---|---|
| Weylus | AGPL-3.0+ (BSD-3 contributions) | **No** | No (only an AGPL combined work) | Yes. A separate client or server speaking its WebSocket/JSON protocol is a separate work. |
| Sunshine, moonlight-common-c, moonlight-ios, moonlight-qt | GPL-3.0 | **No** | **No** (linking moonlight-common-c makes the binary GPL) | Yes. A clean-room implementation of the GameStream pen packet semantics is fine. |
| Deskreen, RustDesk, Tandem | AGPL-3.0 | No | No | Yes |
| KRdp (most files), KPipeWire | LGPL-2.1/3.0 (KDE-accepted) | No copying into our tree (it would make those files LGPL) | **Yes, dynamic linking** with LGPL obligations (allow relinking, ship notices) | n/a |
| KRdp `VideoStream.cpp` | GPL-2.0+ | No | No | n/a |
| KPipeWire `dmabufhandler.cpp` | Apache-2.0 | Yes (keep NOTICE and license) | Yes | n/a |
| plasma-wayland-protocols XML | LGPL-2.1+ | Use the MIT `wayland-protocols-plasma` crate (generated bindings); do not vendor the XML into MIT sources | n/a | Yes |
| usbmuxd daemon / iproxy | GPL-2.0(+) | No | Not needed: talk to the daemon over its socket | Yes (the plist protocol) |
| libusbmuxd, libimobiledevice | LGPL-2.1 | No | Yes (dynamic) | Yes |
| inputtino, libvirtualhid (library and Linux backend), InkLink, GfxTablet, PeerTalk, idevice, go-ios, Tether, evdi (MIT parts), wayland-protocols-plasma, ashpd, pipewire-rs, reis | MIT (or MIT/Apache) | **Yes**, with attribution | Yes | Yes |
| libvirtualhid Windows driver and macOS broker | LicenseRef-LizardByte-SAL-1.0 | No | Avoid | n/a |
| OpenTabletDriver | LGPL-3.0 | No | Possible (.NET) but not needed | Yes |
| InkWand | none (all rights reserved) | **No** | No | Ideas only |

**App Store note:** GPL-licensed iOS apps have a history of conflict with App Store terms. Apple removed GNU Go in 2010 after an FSF complaint and VLC in 2011 ([The Register](https://www.theregister.com/2010/05/27/gnu_go_fsf_apple_itunes/), [Engadget](https://www.engadget.com/2011-01-09-the-gpl-the-app-store-and-you.html)). Moonlight iOS is published on the App Store by its own project ([README](https://github.com/moonlight-stream/moonlight-ios/blob/master/README.md)), which works only because its copyright holders allow it. **An MIT or Apache-2.0 iPad app has no such issue**, which is another point in favour of option (b). Shipping a modified Moonlight iOS fork on the App Store would carry this risk.

---

## 7. Recommendation

### Option (a): native iOS client for an existing server, contributed upstream
- **Weylus:**
  - *For:* its event model already looks like PointerEvent; there is uinput pen, touch and keyboard; a KDE portal capture exists; and no native iOS client exists, so the need is real.
  - *Against:*
    - AGPL.
    - Effectively unmaintained upstream: one merged PR since 2025, 16 open PRs, and the last release in 2021.
    - Most of our requirements need **server and protocol** work, not client work: distance, barrel roll, stylus buttons, squeeze and double-tap, a haptics back-channel, KWin virtual outputs, output mapping, zero-copy VA-API, dialling out over usbmux, bitrate and IDR control, versioning and TLS (§1.9).
    - fMP4-over-WebSocket is awkward for a native decoder.
  - **Verdict: not recommended.** At most, a usbmux bridge plus a native client could be offered to Weylus users later as a side project.
- **Sunshine plus Moonlight:**
  - *For:* production-grade low-latency video (VA-API with IDR on demand, FEC, KWin capture); pen pressure, tilt, azimuth and hover distance already end to end; active maintainers.
  - *Against:*
    - GPL-3.0.
    - Game-streaming product focus; Linux virtual displays were explicitly declined.
    - **UDP transport is incompatible with usbmuxd**, so USB needs a git usbmuxd NCM setup.
    - Barrel roll, squeeze, double-tap and haptics need a coordinated change across the moonlight-common-c wire format, Sunshine and all clients.
    - Known Pencil latency and pressure bugs on iOS (#694, #638).
    - Open KWin plus VA-API black-screen bug on AMD (#6009).
  - **Verdict: use it as a baseline and benchmark, not as the base.** Sunshine (kwingrab) + Sunveil/krfb-virtualmonitor + Moonlight iOS already provides "a virtual monitor plus a pressure-sensitive pen" on Plasma 6 today. Measure our latency against it.

### Option (b): a new MIT/Apache-2.0 project (recommended)
Reasoning:
1. The core of what we need is assembled from permissively licensed parts plus KDE APIs:
   - zkde_screencast virtual outputs (`wayland-protocols-plasma`, MIT), with a portal `VIRTUAL` fallback (`ashpd`, MIT);
   - KWin `InputDevice.outputName` mapping (D-Bus);
   - a uinput pen with `ABS_DISTANCE` and `ABS_Z`, following inputtino, libvirtualhid, OpenTabletDriver and InkLink conventions (MIT code available);
   - the PipeWire DMA-BUF → VA-API H.264/HEVC zero-copy chain (an idea from KPipeWire, implemented with GStreamer `va` elements or libav ourselves);
   - usbmuxd `Connect` to an iPad-side `NWListener` (InkLink-style, or the `idevice` crate);
   - Bonjour for Wi-Fi.
2. None of the existing protocols carries barrel roll, squeeze, double-tap **and** a haptics back-channel. Designing our own versioned protocol is cheaper than coordinating changes to Weylus or GameStream.
3. A permissive license keeps App Store distribution and future contributions simple.

Concrete reuse plan:
- **Take a close look at InkLink first (MIT).** It is 2 days old but already implements most of the architecture: iPad as server, usbmux, Bonjour, Fit/Fill/Virtual modes, coalesced Pencil events, roll, squeeze and double-tap, and a uinput pen with `ABS_Z`. Options are to **fork it with attribution, borrow its usbmux and protocol modules, or propose collaboration**. Its gaps against our goals:
  - no hover distance and no haptics;
  - no KWin-specific virtual output sizing or output mapping;
  - TCP-only video without IDR or bitrate adaptation beyond a KEYFRAME request;
  - repo hygiene (a vendored Windows driver DLL and stray cache files).
- **uinput pen device:** start from inputtino `PenTablet` or the libvirtualhid Linux backend (MIT) and add `ABS_Z`, using the libinput Art-Pen convention for barrel roll. Use tilt resolution in units per radian (e.g. 57 to get about 1 unit per degree), and give `ABS_X`/`Y` a resolution derived from the iPad's physical size so libinput reports correct mm.
- **Ideas only (GPL/AGPL):**
  - Sunshine: infinite GOP with IDR on demand and RFI; `async_depth=1`; the polar-to-Cartesian tilt maths; the KWin `.desktop` permission approach.
  - moonlight-ios: angle conventions; the hover recogniser pattern.
  - Weylus: the hover-leave workaround.
  - KRdp: the `"Virtual-"` mapping id; EIS usage.
- **Plan for the future:** migrate injection from uinput to libei stylus (`reis`) once libei 1.7 ships and KWin's EIS exposes `EI_DEVICE_CAP_STYLUS`. Optionally offer USB-NCM networking (via a git usbmuxd) as a second USB mode for UDP/QUIC transport.

---

## Appendix A: verification commands used
- `gh api repos/OWNER/REPO --jq '.pushed_at,.license.spdx_id,.stargazers_count,.archived'`, `gh api repos/OWNER/REPO/commits?per_page=N`, and `gh api repos/OWNER/REPO/releases/latest` for every GitHub project listed.
- `gh api repos/OWNER/REPO/contents/PATH` to read individual source files: Weylus protocol, uinput and encoder files; moonlight-common-c `Limelight.h`/`Input.h`; moonlight-ios `StreamView.m`; Sunshine `virtualhid*.cpp`, `kwingrab.cpp`, `portalgrab.cpp`, `video.cpp`; inputtino `pentablet.cpp`; libvirtualhid `uhid_backend.cpp`; KWin `screencastmanager.cpp`, `drm_backend.cpp`, `drm_virtual_output.cpp`, `libinput/device.h`; InkLink sources; usbmuxd `usb.c`; libusbmuxd `usbmuxd-proto.h`; PeerTalk `PTProtocol.m`.
- GitLab REST, e.g. `https://invent.kde.org/api/v4/projects/plasma%2Fkrdp/repository/commits` and `…/tags`, plus raw files from invent.kde.org, gitlab.freedesktop.org (libei, libinput) and gitlab.gnome.org.
- Arch package versions: `https://archlinux.org/packages/search/json/?name=…`.
- Crate licenses: `https://crates.io/api/v1/crates/<name>`.
- Apple API availability: `https://developer.apple.com/tutorials/data/documentation/<path>.json`.

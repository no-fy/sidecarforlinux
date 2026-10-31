# Phase 2: virtual display (design)

Status: **design + first implementation**. See §5 for what is tested.
- **Tested end to end:** the video channel, Annex-B → AVCC, iPad decoding (VideoToolbox, macOS) and a live x264 test pattern.
- **Built but untested:** creating the KWin virtual monitor, PipeWire capture with VA-API encoding, and display on the iPad. The wire messages are already specified in [protocol.md §6](protocol.md#6-messages-video-channel-phase-2-specified-not-implemented) and covered by the golden vectors. The research with sources is in [research/kde-display-encode.md](research/kde-display-encode.md).

## Goal

An extra monitor on KDE Plasma Wayland, at the iPad's native resolution, streamed to the iPad with the pen automatically mapped onto it.

| Model | Native resolution | Scale | Logical size | Refresh |
|---|---|---|---|---|
| iPad Air 11″ (M2–M4) | 2360×1640 | 2 | 1180×820 | 60 Hz |
| iPad Air 13″ (M2–M4) | 2732×2048 | 2 | 1366×1024 | 60 Hz |

Target: **< 40 ms** end to end over USB, measured rather than assumed.

## 1. Creating the virtual monitor: options and choice

| Option | Root? | Exact size + scale | Notes |
|---|---|---|---|
| **A. KWin `zkde_screencast_unstable_v1.stream_virtual_output_with_description`** (v4, Plasma ≥ 6.3) | no | ✅ both | KRdp `--virtual-monitor` uses it. Deterministic output name `Virtual-<name>`, which shows up in Display Settings and the Drawing Tablet KCM. Destroyed when the stream closes. KWin ≥ 6.8 only hides it from sandboxed apps; on ≤ 6.7 the binary needs a `.desktop` file with `X-KDE-Wayland-Interfaces=zkde_screencast_unstable_v1`. |
| B. xdg-desktop-portal ScreenCast, `types = VIRTUAL (4)` (Plasma ≥ 6.5) | no | size only (≥ 6.8 via PipeWire size negotiation); scale fixed at 1 | Standard (GNOME too). Shows a dialog once, then a restore token. |
| C. EDID-forced spare connector (`video=…e`, `drm.edid_firmware`) | yes + reboot | ✅ | Needs a free port and may fail link training on the MPX modules. |
| D. evdi (DKMS) | yes | ✅ | DKMS must rebuild against every t2 kernel; only verified up to kernel 6.15; adds a CPU copy. |
| E. vkms | modprobe | partly | Software DRM device, extra copy. |

**Choice: A, with B as automatic fallback.**
- No root and no kernel or DKMS work, which matters on t2linux.
- Exact size, scale and name; KDE's own KRdp follows the same path.
- Both A and B go through the same KWin `ScreenCastStream`, so latency is identical.

**Undo:** the capture module only needs a PipeWire node id, so either backend can be swapped.

## 2. Pipeline

```
KWin ──zkde stream_virtual_output_with_description("ipad", 2732×2048, scale 2, cursor=metadata)──▶ PipeWire node
  │  (output "Virtual-ipad"; daemon sets the pen's KWin outputName = "Virtual-ipad" over D-Bus)
  ▼
sidecard capture (pipewire-rs): DMA-BUF + modifiers, SHM fallback; drop SPA_CHUNK_FLAG_CORRUPTED
  (cursor-only) buffers; cursor position/bitmap sent separately, so moving the cursor doesn't re-encode
  ▼
VA-API encode (zero copy): DMA-BUF → VA surface → vapostproc RGB→NV12 → H.264
  GStreamer: pipewiresrc ! vapostproc ! vah264enc b-frames=0 rate-control=cbr target-usage=3
             key-int-max=0(IDR on request) ! appsink sync=false     (AMD_DEBUG=lowlatencyenc)
  or FFmpeg/libav: h264_vaapi -bf 0 -async_depth 1 -rc_mode CBR -quality 0 (as KPipeWire does)
  software fallback: x264 tune=zerolatency speed-preset=superfast, sliced threads
  ▼
Annex-B → AVCC (4-byte BE lengths) per access unit; SPS/PPS → VideoConfig
  ▼
Video channel (separate TCP+TLS connection, channel=1, same session_id), one AU per VideoFrame
  ▼
iPad: CMVideoFormatDescriptionCreateFromH264ParameterSets → CMSampleBuffer (DisplayImmediately)
      → AVSampleBufferDisplayLayer.sampleBufferRenderer.enqueue (iOS 17+; new API on iOS 27)
      or VTDecompressionSession (no temporal processing) → CAMetalLayer
      pen capture view stays on top; local cursor overlay from cursor metadata
  ▼
VideoFeedback (received/decoded/displayed timestamps, keyframe requests on decode errors)
```

**Your machine** (`doctor.sh`, 2026-10-30) is a MacBook Pro 16" 2019 with two VA-API encoders:
- Intel UHD 630 (iHD driver): H.264 and HEVC encode, including the low-power `EncSliceLP` entrypoint. `vainfo` picked this one by default, which suggests KWin composites on the Intel GPU. **Assumption:** encoding on the same GPU avoids a cross-GPU copy of every frame, so try Intel first.
- Radeon Pro 5300M/5500M (Navi 14, Mesa radeonsi): H.264 and HEVC encode.
- GStreamer's `va` plugin names the first render node's encoder `vah264enc` and the second `varenderD129h264enc`; which one is Intel is not yet known.

**Codec on the Mac Pro 2019 GPUs** (from the original brief, kept for reference; Radeon Pro 580X / Vega II / W5x00X / W6x00X; Mesa radeonsi):
- H.264 encode works on all of them.
- HEVC encode works on all of them, but without B-frames, which we don't want anyway.
- There is no low-power entrypoint and no AV1 encode.
- **Start with H.264**, Main/High, 0 B-frames, CBR with a 1–2 frame VBV, IDR only when requested.
- `scripts/doctor.sh` prints `vainfo` so we know which GPU you have.

## 3. Latency budget (estimate, to be measured)

| Stage | Estimate |
|---|---|
| App renders into the virtual output | 0–16.7 ms (application) |
| KWin composite on the virtual output's 60 Hz software vsync | 0–16.7 ms |
| PipeWire hand-off (DMA-BUF) | < 1 ms |
| VA-API encode 2732×2048 H.264 | 3–8 ms |
| USB transport (~150–400 kB keyframes, ~20–60 kB P-frames at 30–50 Mb/s) | 1–4 ms |
| VideoToolbox decode | 2–5 ms |
| iPad display (60 Hz) | 0–16.7 ms |
| **Total glass-to-glass (pen → photons on the iPad)** | **~30–60 ms** |

**Ways to get under 40 ms:**
- Try a 120 Hz custom mode on the virtual output: `kscreen-doctor output.Virtual-ipad.addCustomMode.2732.2048.120000`. Untested.
- Encode immediately on buffer arrival.
- Use `DisplayImmediately`, or a CAMetalLayer that presents as soon as decode finishes.
- Optionally reduce to 1366×1024 at scale 1 for a lower encode cost.

### Measuring end to end

1. **Per-stage timestamps** that are already in the protocol: `capture_ns` and `encoded_ns` (daemon clock), `received_ns`, `decoded_ns` and `displayed_ns` (iPad clock, converted with the same Ping/Pong offset as the pen path). `sidecard latency --video` will print the breakdown.
2. **Pen to iPad pixels:** the iPad app timestamps a pencil sample. The daemon injects it, and a test client (a tiny Wayland window on the virtual output) changes colour at the pen position. The iPad detects the colour change in the decoded frame. This measures the whole loop without a camera.
3. **Camera check:** a slow-motion phone video (240 fps) of the pencil tip and the line on the iPad.

## 4. Pen mapping onto the virtual display

When the virtual output exists:
- Set `outputName = "Virtual-ipad"` on the pen's KWin InputDevice. This is already implemented as `[kde] output` for phase 1.
- Keep `area = [0,0,1,1]`.

The capture view on the iPad covers exactly the video rectangle, so pen coordinates and pixels line up 1:1 (normalized coordinates are already used).

## 5. Milestones and status

Status labels as in REPORT.md.

1. **Virtual monitor: `sidecard display-probe`. Tested on real hardware (2026-10-30, KWin 6.8.2, your MacBook Pro): `Virtual-ipad` 2360×1640 created.**
   - Creates the output with `zkde_screencast` and prints the PipeWire node id.
   - `--existing NAME` streams an existing output instead.
   - **Tested in the VM** against a headless KWin 6.3.6 (`kwin_wayland --virtual`, `KWIN_WAYLAND_NO_PERMISSION_CHECKS=1`):
     - The protocol binds (KWin offers v5; we use v4) and the request goes out.
     - KWin's `failed` replies are handled. Its virtual backend can't create outputs ("Could not find output"); the DRM backend on real hardware can.
     - Screencasting needs OpenGL compositing, which the container (no `/dev/dri`) can't provide.
2. **Encoder: GStreamer subprocess writing Annex-B to stdout (`daemon/src/videochan.rs`).**
   - **Tested in CI and the VM with the x264 test pattern:** 2360×1640 at 60 fps from GStreamer → our splitter → video channel → fake iPad. CI asserts frames arrive.
   - **Tested on real hardware:** the VA-API pipeline (`pipewiresrc path=<node> ! vapostproc ! vah264enc b-frames=0 rate-control=cbr target-usage=3 …`) on the default VA device of your Intel UHD 630 + Radeon Pro 5300M/5500M laptop. **Untested:** the PipeWire → x264 fallback.
   - **Framing:** the pipeline ends in `h264parse … alignment=au ! gdppay ! fdsink`. Each GDP packet is exactly one frame, so the daemon sends it immediately. With raw Annex-B it had to wait for the next frame's start code; that hid up to 1 s of lag at the end of strokes, found on the real machine.
   - **Why a subprocess** (DECISIONS D18): no GStreamer bindings in the daemon, and the encoder is easy to swap and debug with `gst-launch-1.0` alone.
3. **Annex-B → AVCC access units + parameter sets (`daemon/src/video.rs`). Tested.**
   - Unit tests cover any chunking, H.264 and HEVC.
   - A golden vector built from a real x264 clip is decoded by **VideoToolbox on macOS** in CI.
4. **Video channel. Tested.**
   - A second TLS connection carries `channel=Video` and the same `session_id`. The iPad accepts it only from the PC that owns the active pen session, and closes it when the pen session ends.
   - Tested daemon ↔ fake iPad (Rust) and Swift `PeerServer` ↔ TestDaemon.
   - **Tested against the real Rust daemon on macOS:** it streams the clip, and VideoToolbox decodes all 8 frames.
5. **iPad display: `VideoView`, an `AVSampleBufferDisplayLayer` with DisplayImmediately, under the transparent pencil surface. Tested on your iPad Air.**
6. **`sidecard run --video virtual`. Tested on real hardware.** Encoder out → iPad received: p50 about 1.2 ms over USB. The first attempt failed for an unknown reason; it worked after restarting the app and replugging.
   - Creates the virtual monitor at the iPad's pixel size (surface points × scale).
   - Pins the pen to it through KWin D-Bus.
   - Captures and encodes it, then streams it.
   - MORNING.md §10 has the commands.
7. **Latency work (from the real machine, 2026-10-30).**
   - **Measured:** pen input → frame encoded is about 33 ms median (KWin's 60 Hz capture tick, KWin re-rendering the scene for the capture, colour conversion, encoding); encoder out → iPad is under 1 ms over USB.
   - **Changes:**
     - the iPad queues frames on the network queue instead of the main queue, where 240 Hz pencil touches could delay them by up to a display refresh;
     - VA-API `target_usage` defaults to 7 (fastest);
     - new options `[video] encoder = "vaapi-lp"` (Intel fixed-function encoder) and `[video] scale` (fewer pixels).
   - **Not changeable from our side:** the virtual output's 60 Hz (the zkde_screencast protocol has no refresh-rate argument).
8. **iPad decode + display: measured by a second renderer (DECISIONS D20, D21). Tested on your iPad Air M4.**
   - ⚙︎ → Video renderer → **Metal**: the app decodes with VideoToolbox, draws the newest frame with Metal and presents it immediately. It reports when each frame was decoded and when it reached the screen.
   - The daemon logs `video on the iPad (p50/p95 ms, …)` next to the existing lines ([latency.md](latency.md#video-phase-2)).
   - **AVSampleBufferDisplayLayer** (now the fallback) can't report either time.
   - **Tested on your iPad Air M4 (2026-10-31):** frames reach the screen; decode 1.9 ms, decoded → on screen ≈ 40 ms p50. Metal felt better than AVSampleBufferDisplayLayer and is now the default.
   - **Round 2:** three ways to present (display link, UI update link, present at once), see DECISIONS D21; built but untested on a device, MORNING.md §13.
9. **Mirroring: done by KDE.** In System Settings → Display, set `Virtual-ipad` as "Replica of" another screen. Tested on your machine. If the video drops while you change display settings, the daemon restarts the channel by itself.
10. **Native capture + encode in the daemon (DECISIONS D22, [research/native-video.md](research/native-video.md)).** `[video] backend = "native"` or `sidecard run --video virtual --video-backend native`. The GStreamer subprocess stays the default.
   - PipeWire consumer (pipewire-rs): DMA-BUF with the modifiers VA-API allocates, SHM fallback, drain to the newest buffer, KWin's capture tick from the buffer header. **Tested** with PipeWire 1.4 (VM) and 1.0.5 (CI) and a synthetic node: SHM negotiation, fresh pictures with a slow consumer, keyframe on request. DMA-BUF from KWin: **built but untested**.
   - VA-API: DMA-BUF import, VPP RGB → NV12, H.264 CBR with our own packed headers (cross-checked with FFmpeg's parser), IDR on request. **Built but untested** (no GPU here).
   - Per-stage log line every 10 s: capture tick → daemon, convert, encode, send, and pen input → capture tick ([latency.md](latency.md#where-the-pcs-time-goes-native-backend)).
   - `backend = "hybrid"`: native capture piped into the gst encoder. Slower; it tests the capture half on its own. **Tested** end to end against the fake iPad in the VM and in CI.
11. **Still to do:**
   - cursor as metadata, drawn on the iPad (currently embedded in the frames; OpenDisplay's separate cursor channel is the model);
   - `sidecard latency --video` (the stats are in the `run` log for now);
   - the 120 Hz custom mode: KWin fixes the stream's max framerate when the stream is created, so a new `stream_output` on the output after switching it to 120 Hz is the next experiment (research/native-video.md §1);
   - a Wayfire/wlroots capture source (`wlr_export_dmabuf`) feeding the same encoder (research/native-video.md §4);
   - glass-to-glass measurement.

## 6. Open questions for the real machine

- Does `zkde_screencast` v4 bind from an unsandboxed Rust client on your Plasma version without a `.desktop` file?
- Which render node is the Intel GPU, and do KWin's BGRx DMA-BUF modifiers import into VA-API on it? (`ls -l /dev/dri/by-path`, then `gst-inspect-1.0 va`.)
- Does KWin remember the virtual output's placement across sessions?

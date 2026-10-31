# Latency

## What is measured

```
 Pencil tip ─▶ iPad digitizer ─▶ UIKit (coalesced 240 Hz) ─▶ app ─▶ TLS/TCP ─▶ usbmux/USB (or Wi-Fi)
            ─▶ sidecard ─▶ write(/dev/uinput) ─▶ libinput/KWin ─▶ app (Krita) ─▶ GPU ─▶ display
            ╰──────────────────────── "touch → uinput" ───────────────────────────╯
```

`sidecard latency` (and the periodic log line of `sidecard run`) reports two numbers.

1. **touch → uinput**
   - **From:** `UITouch.timestamp`, the moment the iPad sampled the pencil.
   - **To:** the moment the corresponding evdev frame was written to `/dev/uinput`.
   - **Covers:** UIKit's event delivery and coalescing (one event per display frame, so up to 16.7 ms on the iPad Air's 60 Hz screen), the app, the transport, and the daemon.
   - **Doesn't cover:** the compositor, the application or the screen.
2. **network only**
   - **From:** the moment the iPad handed a batch to the network stack.
   - **To:** the moment the daemon received it.

### How the clocks are compared

- The two devices have different clocks. The daemon runs NTP-style Ping/Pong exchanges.
- It takes the exchange with the smallest round-trip time among the last 16 to estimate the offset.
- Error bound: ± RTT/2. The report prints the RTT; expect well under 1 ms over USB.
- Drift: the offset is refreshed every second, so clock drift (tens of ppm) stays under 0.1 ms.

### Video (phase 2)

```
 pen ─▶ … uinput ─▶ KWin redraw ─▶ capture (60 Hz) ─▶ encode ─▶ USB ─▶ iPad: received ─▶ decoded ─▶ on screen
        ╰──── "pen input -> frame encoded" ────────────────╯  ╰─ transport ─╯  ╰ decode ╯ ╰ wait for screen ╯
```

With `--video`, `sidecard run` logs three lines every 10 s (from the iPad's `VideoFeedback` timestamps and the pen channel's clock offset):

| Log line | Measures |
|---|---|
| `video latency (pen input -> frame encoded, after a pause)` | The PC's share: KWin redraw, the virtual monitor's capture tick, colour conversion, encode. |
| `video latency (encoder out -> iPad received)` | USB/Wi-Fi + TLS. Under 1 ms over USB. |
| `video on the iPad (p50/p95 ms, N frames in T s): decode … \| decoded -> on screen … \| received -> on screen … \| encoder out -> on screen … \| R replaced by a newer frame, F not decoded` | The iPad's share, with the **Metal** renderer only (the default; ⚙︎ → Video renderer). |

How to read the iPad line:
- **decode**: last byte received → VideoToolbox handed back the picture.
- **decoded -> on screen**: until the frame was actually shown: wait for a free drawable, GPU draw, then the wait for the panel's next refresh. The end point is the drawable's `presentedTime`, the moment iPadOS says it reached the display. On a 60 Hz iPad, expect 0–16.7 ms of this to be the refresh wait alone.
- **received -> on screen** is the two together; **encoder out -> on screen** adds the transport (and needs clock sync, ± rtt/2).
- decode and the wait for the screen are differences of two iPad timestamps, so clock sync doesn't affect them.
- **N frames in T s** is the frame rate the PC really sent while the screen kept changing. KWin sends nothing when nothing moves, plus a keep-alive every second. About 600 per 10 s while drawing means 60 fps; 1200 would mean a 120 Hz capture.
- **replaced by a newer frame**: decoded but never shown, because a newer one was ready first. The Metal renderer always shows the newest frame. If the PC sends more than 60 fps, the 60 Hz iPad can't show them all, so this number grows.
- **not decoded**: decode errors, or frames before the first keyframe after a renderer switch. The iPad asks for a keyframe. With `backend = "native"` the daemon makes one at once (log: `iPad could not show frame N: asking the encoder for a keyframe`); the GStreamer backend can't (D18), so the iPad waits for the next one, every 2 s.

With **AVSampleBufferDisplayLayer** (the fallback renderer), the line says the times aren't reported. That layer decodes and displays inside iPadOS and has no per-frame callback for either, so decode and display time are unknowable there with public API. Comparing it with Metal is therefore by feel, or with a camera (below), plus the Metal numbers showing how much the iPad side can cost at all.

Rough total, pencil to iPad screen: touch → uinput (≈ 9 ms p50) + pen input → frame encoded (≈ 33 ms measured) + encoder out → on screen (Metal line). That's a sum of medians, not a measured distribution. With the Metal renderer the daemon prints the last two added up: `video latency (pen input -> on the iPad screen, sum of medians) about N ms`.

### Where the PC's time goes (native backend)

With `[video] backend = "native"` (or `--video-backend native`), two more lines say where "pen input -> frame encoded" goes:

```
video stages p50/p95 ms over N frames: capture tick -> daemon A, convert B, encode C, send D; tick -> sent E; S stale buffers skipped; peak F frames/s
video latency (pen input -> capture tick, after a pause) p50 … ms p95 … ms over N pauses
```

| Part | From → to | What it contains |
|---|---|---|
| pen input → capture tick | uinput write → KWin's capture tick for the frame that shows it | Krita drawing the stroke, KWin compositing, and the **wait for KWin's next capture tick**. KWin ticks on a fixed 60 Hz grid (`VsyncSource::arm`), so 0–16.7 ms of this is that wait, 8.3 ms on average. |
| capture tick → daemon | the tick (`spa_meta_header.pts`, CLOCK_MONOTONIC) → the buffer reached sidecard | KWin re-rendering the scene into the capture buffer, PipeWire hand-off. |
| convert | buffer dequeued → VA-API colour conversion done (KWin gets the buffer back here) | DMA-BUF import (cached) + BGRx → NV12 on the GPU. Also waits for KWin's rendering to finish (implicit sync). |
| encode | conversion done → H.264 bytes in hand | VA-API encode. |
| send | bytes in hand → written to the iPad's socket | AVCC packaging, TLS. |
| **stale buffers skipped** | | Older pictures dropped because a newer one was already waiting: only the newest is encoded. |
| **peak frames/s** | | The busiest second: the real capture rate while something keeps changing (60 at most while KWin ticks at 60 Hz). |

The two pen lines don't share the same pauses exactly (frames captured before the input don't count for "capture tick"), but their medians add up: input → tick + tick → sent ≈ input → encoded + send.

`hybrid` logs the same line with "copy out" (copy out of PipeWire) and "gst encode" (pipe + gst-launch) in the middle.

### What isn't measured (yet)

**Glass-to-glass** latency, from tip to photons on the PC screen, needs a camera. Once phase 2 works it also needs the video path (PC → iPad). Planned method: a high-speed phone video of the pencil moving across the iPad while the PC screen shows the stroke, then count frames. See [phase2-virtual-display.md](phase2-virtual-display.md#measuring-end-to-end).

## Results

| Date | Setup | touch → uinput p50 / p95 / p99 | network p50 / p95 | RTT | Status |
|---|---|---|---|---|---|
| 2026-10-30 | CI ubuntu runner, **fake iPad** over TCP+TLS loopback, 240 Hz samples batched at 120 Hz | 5.34 / 9.91 / 10.22 ms | 0.08 / 0.10 ms | 0.09 ms | **tested** (CI artifact `uinput-evidence/latency-loopback.txt`) |
| 2026-10-30 | MacBook Pro 16" (t2linux 7.2.11, KDE 6.8.2) + iPad Air (60 Hz display) + Pencil Pro over **USB** (usbmuxd), 30 s of drawing, 6786 samples in 1729 batches | 8.62 / 16.33 / 16.66 ms (max 17.19) | 0.68 / 1.00 ms | 0.69 ms | **tested** (your run) |
| 2026-10-30 | Phase 2 video, same setup, `--video virtual`, VA-API H.264 2360×1640, 20 Mbit/s: **encoder out → iPad received** | p50 1.1–1.8 ms, p95 2.5–3.7 ms (4546 frames) | | 0.5–1.0 ms | **tested** (your run). Excludes capture, encode, decode and display. |
| 2026-10-31 | iPad Air **M4**, iPadOS 27, USB, KDE custom mode 2360×1640@120 (747–1135 frames per 10 s), **Metal, present at once** (round 1): received → decoded → on screen | decode 1.9 / 2.3 ms; **decoded → on screen 38–46 / 50–60 ms**; received → on screen 40–48 / 52; encoder out → on screen 41–49 / 53 (p50 / p95, about 10 windows); ≈ 50 % replaced by a newer frame | | | **tested** (your run). pen input → frame encoded also dropped to 17–21 ms p50 at 120 Hz (was 33). |
| — | Same, Metal **display link** / **UI update link** (DECISIONS D21) | *to be measured: MORNING.md §13* | | | built but untested |
| — | MacBook Pro 16" + iPad Air over **Wi-Fi** | *to be measured: MORNING.md step 9* | | | not measured |

How to read the USB row:
- The histogram has four peaks about 4.2 ms apart: 3–4, 7–8, 11–12 and 15–17 ms. That's 240 Hz pencil sampling with delivery at 60 Hz. The iPad Air's display is 60 Hz, so iPadOS hands the app about 4 coalesced samples per frame: 1729 batches in 30 s ≈ 58 per second, 3.9 samples each.
- The **freshest** sample of each batch reaches uinput about 3.5 ms after the digitizer measured it. The older samples in the same batch are 4, 8 and 12 ms older. They still help, because they're the in-between points that make strokes smooth.
- USB plus TLS plus the daemon cost about 0.7 ms (network p50). Nearly all the rest is iPadOS's 60 Hz delivery, which no app can change on a 60 Hz iPad. Your verdict: the lag feels good.
- Not measured here: KWin, Krita, and the PC display.

How to read the loopback row:
- The network cost of TLS over TCP plus the daemon is about 0.1 ms.
- The 5 ms median is almost entirely the fake iPad's simulated coalescing: samples wait for the next 120 Hz "frame".
- On a real iPad, expect the coalescing wait (0–16.7 ms at 60 Hz) plus the USB transfer and the usbmux/lockdown hop. The hop's cost is unknown until measured; it's an **assumption** that it adds well under 1 ms.

## Reproduce

```bash
# real iPad (USB)
sidecard latency --duration 30 --json latency.json
# no iPad: fake iPad on loopback
sidecard fake-ipad --listen 127.0.0.1:47811 --state-dir /tmp/fake &
SIDECAR_CONFIG_DIR=/tmp/pc sidecard pair --connect 127.0.0.1:47811 --dry-run   # answer y, then Ctrl-C
sidecard fake-ipad --listen 127.0.0.1:47811 --state-dir /tmp/fake &
SIDECAR_CONFIG_DIR=/tmp/pc sidecard latency --connect 127.0.0.1:47811 --dry-run --duration 5 | grep -v '^Pen:'
```

## Levers if it's too slow

- **Video renderer (⚙︎ → Video renderer):** Metal (default) decodes with VideoToolbox in the app and always shows the newest frame. How it hands frames to the screen is selectable (display link, UI update link, present at once), because that hand-off is most of the iPad's ~40 ms; see DECISIONS D21. On the iPad, the readout under the latency line compares the modes without the PC.

- **Predicted touches:** the iPad app can send UIKit's predicted samples (⚙︎ → "Send predicted touches") and the daemon can inject them (`use_predicted = true`). This hides about one frame of coalescing delay, at the cost of occasional overshoot.
- **ProMotion iPads (120 Hz):** they halve the coalescing wait. The iPad Air is 60 Hz.
- **Transport:** USB beats Wi-Fi. TCP_NODELAY is set on both ends.

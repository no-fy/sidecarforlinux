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

### What isn't measured (yet)

**Glass-to-glass** latency, from tip to photons on the PC screen, needs a camera. Once phase 2 works it also needs the video path (PC → iPad). Planned method: a high-speed phone video of the pencil moving across the iPad while the PC screen shows the stroke, then count frames. See [phase2-virtual-display.md](phase2-virtual-display.md#measuring-end-to-end).

## Results

| Date | Setup | touch → uinput p50 / p95 / p99 | network p50 / p95 | RTT | Status |
|---|---|---|---|---|---|
| 2026-10-30 | CI ubuntu runner, **fake iPad** over TCP+TLS loopback, 240 Hz samples batched at 120 Hz | 5.34 / 9.91 / 10.22 ms | 0.08 / 0.10 ms | 0.09 ms | **tested** (CI artifact `uinput-evidence/latency-loopback.txt`) |
| — | Mac Pro + iPad Air over **USB** | *to be measured: MORNING.md step 8* | | | not measured |
| — | Mac Pro + iPad Air over **Wi-Fi** | *to be measured: MORNING.md step 9* | | | not measured |

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

- **Predicted touches:** the iPad app can send UIKit's predicted samples (⚙︎ → "Send predicted touches") and the daemon can inject them (`use_predicted = true`). This hides about one frame of coalescing delay, at the cost of occasional overshoot.
- **ProMotion iPads (120 Hz):** they halve the coalescing wait. The iPad Air is 60 Hz.
- **Transport:** USB beats Wi-Fi. TCP_NODELAY is set on both ends.

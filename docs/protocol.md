# sidecarforlinux wire protocol, version 1.0

Reference implementations:
- Rust: [`protocol/rust`](../protocol/rust)
- Swift: [`protocol/swift`](../protocol/swift)

Both are checked against the golden bytes in [`protocol/vectors/vectors.json`](../protocol/vectors/vectors.json). The Rust side generates that file; the Swift tests decode and re-encode every vector on Linux and macOS in CI. If this document and the vectors disagree, the vectors win.

## 1. Roles and transport

| | iPad app | Linux daemon (`sidecard`) |
|---|---|---|
| TCP role | **server** (usbmuxd can only connect *to* the device) | client |
| TLS role | TLS server, requires a client certificate | TLS client, presents a certificate |
| USB | listens on `127.0.0.1:47811` | `usbmuxd` `Connect` to device port 47811 |
| Wi-Fi | listens on `:47812`, advertises Bonjour `_sidecarlinux._tcp` with TXT `fp=<sha256 hex>`, `v=1` | browses mDNS and connects to the advertised host:port |

Each **logical channel** is its own TCP + TLS connection:

| Channel | Id | Carries |
|---|---|---|
| Input | 0 | hello, pairing, pen samples, gestures, haptics, clock sync, latency reports |
| Video | 1 | (phase 2) video config, frames, feedback |

With separate connections, a large video frame never delays a pen sample (no TCP head-of-line blocking). The video channel's `Hello` repeats the input channel's `session_id`.

## 2. Security

- **TLS:** version 1.3 only, ALPN `sidecar/1`.
- **Certificates:**
  - Each side has a long-lived self-signed X.509 v3 certificate with an ECDSA P-256 key.
  - Key usage must include `digitalSignature`, which iOS 18.4+ requires.
  - Extended key usage is serverAuth + clientAuth.
- **Identity:** a peer is identified by its **fingerprint**, the SHA-256 of its DER certificate.
- **Handshake:** signatures are verified normally, so each side proves it holds the key of the certificate it presents. Certificate chains aren't validated, since there's no CA.
- **Trust decision:** right after the handshake, each side looks the peer's fingerprint up in its pinned set.
- **Wi-Fi gate:** the iPad's Wi-Fi listener rejects unknown client certificates *during* the handshake unless the user opened the 2-minute pairing window. Over USB, unknown PCs reach the pairing step (or get refused politely).
- **Unpaired peers:** until both sides trust each other, only `Hello`, `HelloAck`, `Pair*`, `Ping`/`Pong` and `Goodbye` are allowed.

### 2.1 Pairing (numeric comparison with commitment)

Pairing is needed when `Hello.trusts_peer` or `HelloAck.trusts_peer` is false.

```
fp_d = SHA256(daemon cert DER)      fp_i = SHA256(iPad cert DER)

iPad   → daemon  PairCommit(C)      C = SHA256("sidecarforlinux pair commit v1" ‖ fp_i ‖ N_i)
daemon → iPad    PairNonce(N_d)     32 random bytes
iPad   → daemon  PairReveal(N_i)    daemon checks C, aborts on mismatch
both:  SAS = u32_be(SHA256("sidecarforlinux pair sas v1" ‖ fp_d ‖ fp_i ‖ N_d ‖ N_i)[0..4]) mod 1 000 000
       shown as "123 456" on both screens
both → PairDecision(accept)         the user confirms on BOTH devices
```

- **Pinning:** both decisions must be true, then each side pins the other's fingerprint and the session continues as normal. Any `false` ends the session (`Goodbye(PairingRejected)`) and pins nothing.
- **Why the commitment:** the iPad is bound to `N_i` before it sees `N_d`, and the daemon sends `N_d` before seeing `N_i`. A man-in-the-middle, who must run two separate TLS sessions with two different certificates, therefore can't grind keys or nonces until both codes match. A forged pairing succeeds with probability 10⁻⁶ per attempt, and every attempt needs both users to confirm.
- **Daemon behaviour:** `sidecard run` refuses to pair (`Goodbye(NotPaired)`). Pairing needs `sidecard pair` (or `run --pair`) and answers on the PC's terminal.

## 3. Framing

```
frame = length:u32le  type:u8  payload[length - 1]
```

- `length` covers the type byte and the payload. It must be between 1 and 16 MiB.
- All integers are **little-endian**. Floats are IEEE-754 binary32, little-endian.
- **Strings:** `u16 length` + UTF-8 bytes.
- **Blobs:** `u16 length` + bytes.

**Forward compatibility rules:**
1. Unknown message **types** are skipped (the length lets the receiver skip them).
2. Known messages may carry **trailing bytes** added by a newer *minor* version. Receivers ignore them.
3. A payload **shorter** than the v1.0 layout is a protocol error.
4. `PenBatch` carries its per-sample size, so samples can grow too.
5. A different **major** version means: send `Goodbye(VersionMismatch)` and close.

## 4. Clocks

- **Daemon clock (`*_ns` fields the daemon writes):** `CLOCK_MONOTONIC` in nanoseconds.
- **iPad clock:** the `UITouch.timestamp` time base (seconds since boot, excluding sleep), as **nanoseconds since app launch**. It is never raw uptime.
- **Sync:** the daemon estimates `offset = iPad − daemon` from Ping/Pong:
  - `offset = ((t2 − t1) + (t3 − t4)) / 2` and `rtt = (t4 − t1) − (t3 − t2)`
  - It keeps the minimum-RTT sample of the last 16, which bounds the error by rtt/2.
  - Pings go out at 10 Hz for the first second, then at 1 Hz.
- **Liveness:** if a side receives nothing for 5 s, it considers the peer gone. The daemon then releases every pen button and leaves proximity.

## 5. Messages (input channel)

`D→I` means daemon to iPad, `I→D` iPad to daemon. Offsets are within the payload.

### 0x01 Hello (D→I, first message on every connection)
| Off | Type | Field |
|---|---|---|
| 0 | [u8;4] | magic `"SCLP"` |
| 4 | u16 | major (1) |
| 6 | u16 | minor (0) |
| 8 | u8 | channel (0 input, 1 video) |
| 9 | u8 | trusts_peer: daemon has pinned this iPad's certificate |
| 10 | u16 | reserved |
| 12 | u32 | caps (see below) |
| 16 | u64 | session_id |
| 24 | string | daemon name (hostname) |

### 0x02 HelloAck (I→D)
| Off | Type | Field |
|---|---|---|
| 0 | u16 | major |
| 2 | u16 | minor |
| 4 | u8 | trusts_peer: iPad has pinned the daemon certificate |
| 5 | u8 + u16 | reserved |
| 8 | u32 | caps |
| 12 | SurfaceInfo (16 B) | drawing surface |
| 28 | string | device name |
| … | string | model |

**Capability bits:**
- `1<<0` gestures
- `1<<1` haptics
- `1<<2` video
- `1<<3` predicted samples
- `1<<4` roll (Pencil Pro)
- `1<<5` hover
- `1<<6` HEVC decode
- `1<<7` touch: the iPad sends TouchBatch (in HelloAck); the daemon wants it (in Hello). The iPad sends touches only to a daemon that set it.

### 0x03 PairCommit (I→D): `[u8;32]` commitment
### 0x04 PairNonce (D→I): `[u8;32]`
### 0x05 PairReveal (I→D): `[u8;32]`
### 0x06 PairDecision (both): `u8` 1 = accept
### 0x07 Goodbye (both): `u16 code`, `string reason`

**Goodbye codes:**
- 0 normal
- 1 version mismatch
- 2 not paired
- 3 pairing rejected
- 4 protocol error
- 5 busy/replaced
- 6 shutting down

### 0x10 PenBatch (I→D)
| Off | Type | Field |
|---|---|---|
| 0 | u64 | send_time_ns (iPad clock, when handed to the network) |
| 8 | u16 | count |
| 10 | u16 | sample_len (48 in v1.0) |
| 12 | PenSample × count | |

**PenSample (48 bytes in v1.0):**

| Off | Type | Field |
|---|---|---|
| 0 | u64 | timestamp_ns: `UITouch.timestamp` on the iPad clock |
| 8 | u32 | seq (per sample, wraps) |
| 12 | u8 | phase: 0 hover, 1 down, 2 move, 3 up, 4 cancel, 5 out (left proximity) |
| 13 | u8 | flags: bit0 predicted, bit1 coalesced, bit2 pressure estimated, bit3 angles estimated, bit4 roll valid, bit5 from hover recognizer |
| 14 | u16 | reserved |
| 16 | f32 | x: 0..1 across the surface, left → right |
| 20 | f32 | y: 0..1 top → bottom |
| 24 | f32 | pressure: `force / maximumPossibleForce`, 0..1 (0 while hovering) |
| 28 | f32 | altitude, radians (0 = flat, π/2 = upright) |
| 32 | f32 | azimuth, radians in view coordinates (x right, y down), the direction the cap end points |
| 36 | f32 | roll, radians (Pencil Pro `rollAngle`) |
| 40 | f32 | z_offset: hover height 0..1 (`UIHoverGestureRecognizer.zOffset`); 0 when touching |
| 44 | u32 | estimation_index (`UITouch.estimationUpdateIndex`, 0 if none) |

**Semantics:**
- Every coalesced 240 Hz sample is sent; predicted samples only if the user enabled them.
- A stroke is `down`, `move`…, then `up` (or `cancel`).
- Proximity starts with the first `hover`, `down` or `move` and ends with `out`.
- The iPad sends `out` about 60 ms after hover ends (or about 150 ms after a lift with no hover), so stroke starts don't flicker proximity.

### 0x11 PencilGesture (I→D)
| Off | Type | Field |
|---|---|---|
| 0 | u64 | timestamp_ns |
| 8 | u8 | kind: 1 double-tap, 2 squeeze |
| 9 | u8 | phase: 0 discrete (tap), 1 began, 2 changed, 3 ended, 4 cancelled |
| 10 | u8 | bit0: hover pose valid |
| 11 | u8 | preferred_action: `UIPencilPreferredAction` raw value from iPadOS Settings |
| 12 | f32 × 6 | hover pose: x, y, z_offset, altitude, azimuth, roll (zeros if invalid) |

### 0x12 SurfaceInfo (I→D, also inside HelloAck)
| Off | Type | Field |
|---|---|---|
| 0 | f32 | width, points |
| 4 | f32 | height, points |
| 8 | f32 | scale (pixels per point) |
| 12 | f32 | points per inch (132 for 264-ppi iPads) |

The daemon recreates its tablet device when the size changes.

### 0x13 TouchBatch (I→D)
Finger touches; the pencil stays in PenBatch.

| Off | Type | Field |
|---|---|---|
| 0 | u64 | send_time_ns (iPad clock) |
| 8 | u16 | count |
| 10 | u16 | touch_len (24 in v1.0) |
| 12 | TouchSample × count | |

**TouchSample (24 bytes in v1.0):**

| Off | Type | Field |
|---|---|---|
| 0 | u64 | timestamp_ns: `UITouch.timestamp` on the iPad clock |
| 8 | u16 | id: stable from down to up/cancel, may be reused afterwards |
| 10 | u8 | phase: 1 down, 2 move, 3 up, 4 cancel (iPadOS took the touch away) |
| 11 | u8 | flags: bit1 coalesced (as PenSample) |
| 12 | f32 | x: 0..1, as PenSample |
| 16 | f32 | y: 0..1 |
| 20 | f32 | major_radius: `UITouch.majorRadius` in points (palms are much larger than fingers) |

**Semantics:**
- Every coalesced position is sent, in time order. Touches of one UIKit event share a timestamp; the daemon puts them in one evdev frame.
- The iPad sends touches only while it shows the PC's screen (video on) and the user hasn't switched them off.
- Palm rejection is the daemon's: it ignores touches while the pencil is in proximity, and touches above a radius.

### 0x20 Haptic (D→I)
`u8 kind` (1 alignment, 2 path completed), `u8 + u16` reserved, `f32 x`, `f32 y`.

The coordinates are a normalized location; NaN means "where the pencil is". The iPad plays it with `UICanvasFeedbackGenerator`, which reaches the Pencil Pro.

### 0x21 MappingInfo (D→I, informational)
`f32 × 4` area (x, y, w, h normalized), `string` output name.

### 0x30 Ping (D→I): `u32 id`, `u64 t1_ns`
### 0x31 Pong (I→D): `u32 id`, `u64 t1_ns` (echo), `u64 t2_ns` (iPad receive), `u64 t3_ns` (iPad send)
### 0x32 LatencyReport (D→I)
`u32 samples`, `u32 p50_us`, `u32 p95_us`, `u32 p99_us`, `u32 max_us`, `u32 rtt_us`.

This is touch → uinput latency over the recent window; the iPad shows it.

## 6. Messages (video channel, phase 2: specified, not implemented)

### 0x40 VideoConfig (D→I)
`u8 codec` (1 H.264, 2 HEVC), `u8 n_sets`, `u16` reserved, `u32 width`, `u32 height`, `u32 fps_milli`.

These are followed by `n_sets` blobs: the parameter sets without start codes (H.264: SPS, PPS; HEVC: VPS, SPS, PPS). The iPad builds a `CMVideoFormatDescription` from them.

### 0x41 VideoFrame (D→I)
`u32 frame_id`, `u8 flags` (bit0 keyframe), `u8 + u16` reserved, `u64 capture_ns`, `u64 encoded_ns`, then the rest of the frame.

The rest is **AVCC/HVCC**: NAL units each prefixed with a 4-byte **big-endian** length, exactly what `CMSampleBuffer` wants. This is the only big-endian field in the protocol.

### 0x42 VideoFeedback (I→D)
`u32 frame_id`, `u8 flags` (bit0 request keyframe), `u8 + u16` reserved, `u64 received_ns`, `u64 decoded_ns`, `u64 displayed_ns` (iPad clock).

## 7. Linux mapping (informative)

How the daemon turns samples into evdev events is described in [DECISIONS.md D8](DECISIONS.md#d8-uinput-device-layout). In short:
- Tilt: `tiltX = atan2(cos(az)·cos(alt), sin(alt))`, `tiltY = atan2(sin(az)·cos(alt), sin(alt))`, in degrees.
- Roll goes to `ABS_Z`, compensated for libinput's +90°.
- Hover goes to `BTN_TOOL_PEN` = 1 with `BTN_TOUCH` = 0 and `ABS_DISTANCE`.
- Pressure has a 6 % floor while touching, because of libinput's 5 % tip threshold.

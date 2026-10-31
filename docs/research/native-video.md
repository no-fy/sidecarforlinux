# Native capture and encode inside `sidecard`

*Researched 2026-10-31 for branch `feature/native-capture-encode`. Target: your MacBookPro16,1, KWin 6.8.2, PipeWire 1.6.10, Intel UHD 630 (iHD) + Radeon Pro 5300M. Decision: [DECISIONS D22](../DECISIONS.md#d22-native-capture-and-encode-pipewire-rs--raw-libva).*

Labels as everywhere: **tested** (ran in a test or a run whose output I read), **built but untested** (compiles, never ran on real hardware), **assumption** (from documentation or source reading, not verified).

## 0. What and why

Today the second screen is `gst-launch-1.0 pipewiresrc ! vapostproc ! vah264enc ! h264parse ! gdppay ! fdsink` as a subprocess (D18). You measured **pen input → frame encoded ≈ 33 ms** (median), **encoder → iPad ≈ 0.6 ms**. Nobody can say where the 33 ms go, and the subprocess can't make a keyframe on demand.

The native backend does the same work in the daemon:

```
KWin virtual output ──PipeWire (DMA-BUF, zero copy)──▶ sidecard "video" thread
   drain the queue to the newest buffer, skip cursor-only buffers
   import the DMA-BUF as a VA surface (cached per PipeWire buffer)
   VA video processing: BGRx → NV12 (BT.709, limited range)        ← stage "convert"
   hand the PipeWire buffer back to KWin
   VA H.264 encode: CBR, no B-frames, 1 reference, IDR on request    ← stage "encode"
   Annex-B ─▶ the existing AuAssembler ─▶ VideoConfig / VideoFrame ─▶ TLS   ← stage "send"
```

The protocol output is byte-for-byte the same kind of message (`AuAssembler` is reused), so the iPad needs no change.

## 1. Where the 33 ms can go (from KWin's source)

KWin 6.8.2, `src/plugins/screencast/screencaststream.cpp` and `src/utils/vsyncsource.cpp` (tag `v6.8.2`):

1. **App redraw + KWin compositing.** Krita draws the stroke; KWin notices damage on the virtual output.
2. **Waiting for the capture tick.** On damage KWin calls `m_vsync->arm(now)`. `VsyncSource::arm` computes
   `alignTimestamp(max(now, earliest, last + 1ns), 1e12 / refreshRate)`. That is a **software timer on a fixed grid** at the stream's negotiated *max framerate* (`ScreenCastStream::framerate()` = negotiated `maxFramerate`, in mHz). At 60 Hz the wait is 0–16.7 ms, **8.3 ms on average**. Nothing on our side can shorten it except a higher negotiated max framerate.
3. **KWin renders the scene again into the capture buffer** (`record()` → `m_source->render(...)`). For DMA-BUF it only `glFlush()`es (implicit sync, unless the consumer negotiated `SPA_META_SyncTimeline`); for MemFd it reads the pixels back to the CPU.
4. **PipeWire hand-off** to the consumer.
5. **Consumer work:** colour conversion, encode, packaging.

`ScreenCastStream::addHeader` stores the tick time in `spa_meta_header.pts`, from `std::chrono::steady_clock`, which is `CLOCK_MONOTONIC` on Linux, the same clock as the daemon's `now_ns()`. So the native backend can report, per frame:

| Stage | From → to | Source of the timestamps |
|---|---|---|
| tick → daemon | KWin capture tick (`pts`) → buffer dequeued in our `process` callback | KWin header meta, our clock |
| convert | dequeued → VPP finished (`vaSyncSurface`) | our clock |
| encode | VPP finished → coded buffer mapped | our clock |
| send | coded buffer mapped → written to the TLS socket | our clock |

And it splits the existing pen probe in two: **pen input → KWin tick** (app + compositor + waiting for the tick) and **pen input → encoded**. If the first is ~25 ms of the 33, no encoder work will help much; if tick → encoded is large, it will.

**The 120 Hz question (source reading, assumption):** `buildFormats()` offers `maxFramerate` as a range `[0/1 … refreshRate]` with the *output's refresh rate at that moment*. It is rebuilt only when the stream size changes or a DMA-BUF modifier is (re)fixated. The `Virtual-ipad` output is created at 60 Hz by the stream itself, so a 120 Hz custom mode set later doesn't raise the cap of the stream that already exists: KWin keeps ticking at 60 Hz. That matches "no big improvement" at 120 Hz. A second stream opened on the output *after* switching it to 120 Hz (`stream_output(Virtual-ipad)`) should offer 120. The native backend logs the negotiated max framerate (`PipeWire format: … max 60/1 fps`), so this is checkable.

## 2. PipeWire consumer

### Options

| Option | Notes |
|---|---|
| **`pipewire` crate 0.10.1** (pipewire-rs) | MIT, `rust-version = 1.80` (our MSRV). Safe stream API: `dequeue_buffer()` returns a guard that re-queues on drop; listeners for `param_changed`, `process`, `state_changed`; pod builder with `PropertyFlags::MANDATORY \| DONT_FIXATE`. **Build cost:** `libspa-sys` / `pipewire-sys` run **bindgen 0.72** at build time, which needs **libclang** plus the libpipewire/libspa headers. |
| Raw `libpipewire` FFI by hand | Same headers, no bindgen, but the SPA pod builder is all inline C macros: a lot of unsafe re-implementation for nothing. |
| GStreamer `pipewiresrc` (today) | Works, but buffer selection, negotiation and timestamps are out of our hands. |

**Choice: pipewire-rs.** libpipewire and libspa are MIT (Expat) per Debian's `copyright` file; some PipeWire plugins that libpipewire may load are LGPL-2.1+, linked dynamically. No code copied.

### Negotiation

PipeWire's own rules ([`doc/dox/internals/dma-buf.dox`](https://gitlab.freedesktop.org/pipewire/pipewire/-/blob/1.4.2/doc/dox/internals/dma-buf.dox)):

- The **consumer** offers two `EnumFormat` objects per format: one with `SPA_FORMAT_VIDEO_modifier` flagged `MANDATORY | DONT_FIXATE` (an enum of the modifiers it can import), one without (SHM fallback). DMA-BUF ones first.
- The **producer** (KWin, it allocates) fixates the modifier: KWin's `onStreamParamChanged` test-allocates with the intersection and re-announces a fixated format. The consumer sees a second `Format`.
- On the final `Format`: modifier present → `SPA_PARAM_BUFFERS_dataType = 1 << SPA_DATA_DmaBuf`; absent → `MemFd | MemPtr`.
- On import failure, drop that modifier and re-announce (OBS does this; KWin also removes modifiers it couldn't allocate).

**Which modifiers can VA-API import?** libva has no query for it (`VASurfaceAttribDRMFormatModifiers` is allocation-only, `va.h`). GStreamer's VA plugin (`gst-libs/gst/va/gstvaallocator.c`, `gst_va_dmabuf_get_modifier_for_format`) creates a 64×64 VA surface with the VPP usage hint, exports it with `vaExportSurfaceHandle` and offers **that one modifier** per format; that is what `pipewiresrc ! vapostproc` negotiates with KWin on your machine today. We do the same, plus `DRM_FORMAT_MOD_LINEAR` as a second choice. (OBS, KPipeWire and Sunshine ask EGL with `eglQueryDmaBufModifiersEXT` instead; that would add EGL to the daemon for little gain.)

KWin 6.8.2 offers (`buildFormats`):
- DMA-BUF: its output DRM format (normally XRGB8888 → `BGRx`; ARGB8888 is announced as `BGRA` *or* `BGRx`), its EGL modifiers, `MANDATORY | DONT_FIXATE`;
- SHM: `BGRA`/`BGRx`, MemFd;
- size: a range around the output size (virtual outputs follow the negotiated size, since 6.8);
- `framerate = 0/1` (variable) and `maxFramerate` range `[0/1, refresh]`.

We offer `BGRx, BGRA, RGBx, RGBA` (VPP converts all of them), size = the iPad's pixel size, `framerate = 0/1`, `maxFramerate` range with **default = `[video] fps`** (so `fps = 30` really makes KWin tick at 30 Hz).

We don't offer explicit sync (`SPA_META_SyncTimeline`): KWin then falls back to its second Buffers param and implicit sync (`glFlush`), and the driver waits on the DMA-BUF's implicit fences when VPP reads it. **Assumption** (i915 and amdgpu do implicit sync for imported DMA-BUFs).

### Buffer selection

KWin keeps 2–4 buffers (`SPA_PARAM_BUFFERS_buffers` range `3, 2, 4`). If we're slow, several are queued for us. In `process` we **dequeue until the queue is empty and keep only the newest**; older ones go straight back to KWin (counted as "stale buffers skipped"). Buffers whose chunk or header has the `CORRUPTED` flag (cursor-only updates in metadata mode) are skipped. This is the same "latest wins" idea as OpenDisplay's `latest.rs` one-slot channel (below); here PipeWire's queue *is* the slot, because capture and encode run on one thread.

The DMA-BUF is held only until VPP has finished reading it, then re-queued; the encode runs on our own NV12 copy, so KWin can render the next frame while we encode.

## 3. Encoder

### Options

| Option | License / build | Verdict |
|---|---|---|
| **Raw libva via our own bindgen** (narrow allowlist, a thin safe wrapper in `daemon/src/native/va.rs`) | libva and libva-drm are **MIT**. bindgen is already in the build for pipewire-rs, so no new system dependency. | **Chosen.** Full control of VPP, packed headers and IDR. |
| `cros-libva` 0.0.13 (ChromeOS, BSD-3) | bindgen at build time too | Good safe wrapper for surfaces, DRM-PRIME import and H.264 *decode/encode param* buffers, but its bindgen allowlist leaves out `VAProcPipelineParameterBuffer` (VPP) and the packed-header buffer types we need, its objects are `Rc` (single-thread), and it is a 0.0.x API. We would still write raw FFI for half the calls. |
| `cros-codecs` 0.0.6 (BSD-3) | pulls gbm, drm, nix, v4l2r | A whole encoder framework (its own bitstream writer and rate control); no DMA-BUF RGB → VPP path. Too much for one encoder configuration. |
| `ffmpeg-next` + `h264_vaapi` | **Arch's FFmpeg 9.0.2 is configured with `--enable-gpl --enable-version3`** (Arch PKGBUILD), so a daemon linked to it is GPLv3 when distributed. | Rejected for licensing; also bindgen + a large API. |
| `gstreamer-rs` appsrc → vapostproc → vah264enc | LGPL, dynamic | Still GStreamer, still no clean per-stage timing inside the encoder, and keyframes via force-key-unit events. Little gained over the subprocess. |

### Configuration (the "low-delay" part)

Chosen to mirror what both GStreamer `vah264enc` (1.28, `sys/va/gstvah264enc.c`) and FFmpeg `h264_vaapi` (n7.1, `vaapi_encode*.c`, `hw_base_encode_h264.c`) do on iHD, which is the most-exercised path on your driver. Values are ours, written from the H.264 spec (no code copied; both are LGPL anyway).

| Setting | Value | Why |
|---|---|---|
| Entry point | `VAEntrypointEncSlice` (`encoder = "vaapi"`) or `EncSliceLP` (`"vaapi-lp"`) | as today |
| Profile | High if offered, else Main, else Constrained Baseline | VideoToolbox decodes all |
| Level | smallest that fits size × fps (2360×1640 @ 60 → 5.1) | computed, unit-tested |
| GOP | IDR at start and **on request**; P frames otherwise; 1 reference; no B-frames | lowest latency |
| POC | `pic_order_cnt_type = 2` | output order = decode order; FFmpeg does the same without B-frames |
| VUI | BT.709, limited range, `bitstream_restriction` with `max_num_reorder_frames = 0`, `max_dec_frame_buffering = 1` | tells the decoder it may output every frame immediately |
| Rate control | CBR at `[video] bitrate_kbps`, CPB = **0.5 s** of bitrate | GStreamer's default is 2 s (`cpb_size = max_bitrate * 2`); smaller bounds bursts after big changes. Tunable constant. |
| Quality level | `[video] target_usage` (1–7) | as today |
| Packed headers | SPS + PPS (on IDR) and slice headers, when the driver offers them, `has_emulation_bytes = 1`, `slice_qp_delta = 0` | exactly what both reference encoders send under CBR (the driver fixes the QP) |
| Surfaces | VPP output NV12 at the picture size; reconstructed frames at 16-aligned size; SPS cropping | as FFmpeg |

Our bitstream writer (Exp-Golomb, emulation prevention, SPS/PPS/slice header) is unit-tested, and CI cross-checks its output with FFmpeg's `trace_headers` bitstream filter, an independent H.264 syntax parser.

### Keyframes on request

The iPad sets `VideoFeedback.request_keyframe` when it couldn't show a frame. It does that for *every* frame until it gets an IDR, so we de-duplicate: a request is honoured only if no IDR newer than the failed frame is already sent or pending. If the screen is idle (KWin sends nothing), the last converted picture is re-encoded as an IDR, so the iPad recovers without waiting for the next change.

KWin sends nothing while the screen is still, and the iPad only asks for a keyframe when a frame arrives that it can't show. So, like `pipewiresrc keepalive-time=1000` in the GStreamer backend, the last picture is re-encoded after a second without frames (a tiny P frame). Re-encoded pictures are marked as repeats: they stay out of the stage statistics and the pen probe.

## 4. Source-agnostic encode path (for a second compositor later)

The encoder must not care where a DMA-BUF came from. You plan to try **Wayfire 0.11.0** (wlroots 0.20) later, where the source is not PipeWire. Facts you verified in Wayfire's source (not re-checked here, **assumption** for this doc):
- it exposes `wlr_export_dmabuf_manager_v1`, which hands out the output's own rendered buffer every frame (no re-render, no copy), and `ext-image-copy-capture-v1` (`src/core/core.cpp:159-163`);
- IPC `wayfire/create-headless-output {width,height}` creates a wlroots headless output;
- a mode like `2360x1640@120000` becomes a custom mode on non-DRM outputs, and the headless backend renders at that rate (`backend/headless/output.c`, `frame_delay = 1000000/refresh`). Unlike KWin's virtual output, 120 Hz would be real.

So the interface between capture and encode (`daemon/src/native/frame.rs`) is:

```rust
pub enum Pixels<'a> {
    DmaBuf { fourcc: u32 /* DRM */, modifier: u64, planes: &'a [Plane /* fd, offset, pitch */] },
    Shm    { fourcc: u32 /* DRM */, data: &'a [u8], stride: u32 },
}
pub struct Frame<'a> { width, height, pixels: Pixels<'a>, tick_ns: Option<u64>, arrived_ns: u64 }

pub trait FrameSink {
    /// DRM fourcc → modifiers this sink can import (the source offers them to the compositor).
    fn dmabuf_formats(&self) -> Vec<(u32, Vec<u64>)>;
    /// Read the source pixels into the sink's own picture. When this returns,
    /// the source may hand the buffer back to the compositor.
    fn convert(&mut self, frame: &Frame) -> ConvertResult; // Ok | Skipped | ImportFailed{fourcc, modifier}
    /// Encode the last converted picture (IDR if forced or requested).
    fn encode(&mut self, force_idr: bool) -> Result<Option<Encoded>>;
}
```

Pixel formats are **DRM fourccs** (what `wlr_export_dmabuf` and `ext-image-copy-capture` use); the PipeWire source maps SPA video formats to DRM fourccs at its edge. Timestamps are `CLOCK_MONOTONIC` ns: KWin's `spa_meta_header.pts` for PipeWire, the `ready` event's time for wlroots export-dmabuf. The PipeWire source is one implementation (`native/pw.rs`); a Wayfire source would be another, feeding the same `VaEncoder`. The two-call split matters for both: KWin's buffer and a wlroots export frame (`zwlr_export_dmabuf_frame_v1.destroy`) both go back as soon as VPP has read them.

## 5. Threads

One `std::thread` runs the PipeWire main loop; `process` does dequeue → import → VPP → re-queue → encode, and hands Annex-B bytes to the tokio side over a bounded channel (blocking when the socket is slow; encoded P-frames are never dropped, that would corrupt the picture). A `pipewire::channel` carries "keyframe" and "stop" into the loop. VA objects never leave that thread.

Pipelining (VPP of frame N+1 while frame N encodes) would only raise throughput when convert + encode exceeds a frame interval; for one frame's latency, sequential is best. Revisit if the stage numbers say otherwise.

## 6. Fallbacks and the "hybrid" backend

- **No DMA-BUF negotiated** (or import keeps failing): SHM frames are uploaded into a VA surface (`vaDeriveImage` + copy, one CPU copy of ~15 MB/frame) and go through the same VPP + encode.
- **`backend = "hybrid"`:** native PipeWire capture (SHM), raw frames piped into the old `gst-launch-1.0` encoder (`fdsrc ! rawvideoparse ! … ! gdppay`), with a one-slot latest-wins hand-over between capture and the pipe writer. It is slower (CPU copy + pipe) and exists to test the capture half on its own: it is the one path that runs end to end in this VM and in CI, with a synthetic PipeWire node.
- **`backend = "gstreamer"`** stays the default until the native one is proven on your machine.

## 7. Prior art (ideas only)

- **OpenDisplay** (github.com/peetzweg/opendisplay, **GPL-3.0**, read at commit `8c280c8`, 2026-10-29; nothing copied). Linux sender `linux/crates/od-sender`, beta: tested on Hyprland/Asahi; its KDE backend is experimental and hasn't run on a real desktop; CPU x264/openh264 only (VA-API planned), Wi-Fi only, unencrypted.
  - `src/latest.rs`: a one-slot channel where the newest value wins, so a busy encoder always starts on the newest picture. We use the idea for the hybrid pipe writer; in the native path PipeWire's own queue plays that role (drain to newest).
  - `src/pipewire.rs`: SHM-only consumer, `MEDIA_TYPE/CATEGORY/ROLE` stream properties, targets KWin's `object.serial` (zkde v6 `serial` event) in preference to the node id, which PipeWire reuses. We still use the node id we get right after creating the stream (same as `pipewiresrc path=`); switching to the serial is a small follow-up in `display.rs`.
  - `src/encoder/stage.rs`: a CPU convert stage on its own thread so frame N+1 converts while N encodes. Our convert is on the GPU (VPP) and short; see §5.
  - A separate **cursor channel** so the receiver draws the cursor (KWin's `SPA_META_Cursor`). Not in this change; noted for the "cursor as metadata" item in phase2 §5.
  - Their 12–30 ms capture→display numbers are **Mac → Mac** (ScreenCaptureKit + VideoToolbox), not Linux: not used as a baseline here.
- **Sunshine** (LizardByte, GPL-3.0; `src/platform/linux/kwingrab.cpp`, `pipewire.cpp`): native PipeWire consumer that drains to the newest buffer and negotiates variable framerate (`framerate 0/1`, `maxFramerate` as a range; a comment says KWin ≤ 6.7 otherwise paces ~3 % slow). Encodes with FFmpeg VA-API, `async_depth = 1`. Ideas only.
- **KRdp / KPipeWire** (LGPL): DMA-BUF → libav VA-API on KDE, modifiers from EGL.
- **OBS** `linux-pipewire`: modifier renegotiation after a failed import.

## 8. What was tested, and where

| Piece | How | Status |
|---|---|---|
| Bitstream writer (Exp-Golomb, emulation prevention, SPS/PPS/slice header), level choice, IDR schedule, keyframe-request de-duplication | unit tests | **tested** |
| Our SPS/PPS/slice headers read by an independent parser | FFmpeg `trace_headers` (`tests/h264_headers.rs`, High/Main/Constrained Baseline): IDR/P/P/IDR, frame_num 0/1/2/0, crop to 2360×1640, POC type 2, BT.709 limited, no reordering | **tested** (VM, CI) |
| VA parameter structs (sequence, picture, slice, rate control, HRD) consistent with the headers | unit tests on the generated libva structs | **tested** (values only; no driver) |
| Stage timing math, frames/s, pen → capture tick probe, newest-usable selection, modifier bookkeeping, SPA↔DRM formats, EnumFormat pods (modifier flags, DMA-BUF first) | unit tests | **tested** |
| PipeWire consumer against a real PipeWire daemon and a synthetic `Video/Source` node (`gst-launch-1.0 videotestsrc ! pipewiresink`) | `tests/pipewire_capture.rs` with PipeWire 1.4.2 + WirePlumber 0.5 (VM) and 1.0.5 + 0.4 (Ubuntu 24.04 CI, needs a D-Bus session) | **tested**: a sink that offers DMA-BUF modifiers falls back to SHM when the producer can't; a 3× too slow sink still gets fresh pictures (no backlog); a keyframe request gives exactly one IDR, re-encoded from the last picture within 300 ms when idle, from the next frame within 200 ms when busy; with a source that sends one frame per 5 s, the keep-alive re-sends the picture after ~1 s. These tests found a bug (an idle keyframe repeated every 50 ms), now fixed. |
| "Skip older queued buffers" branch | unit test only | gst's `pipewiresink` recycles its buffer instead of queueing several, so a real queue of 2+ never formed in the VM. KWin (driver, 2–4 buffers) may queue; the log's "stale buffers skipped" will tell. |
| `backend = "hybrid"` end to end: PipeWire → our consumer → raw frames → gst x264 → AVCC → video channel → fake iPad | CLI in the VM and CI | **tested** (649 frames, stage line logged) |
| `backend = "native"` without a GPU | VM | **tested**: fails with "no VA-API device can encode H.264 … no /dev/dri/renderD* nodes" and the channel retries |
| DMA-BUF negotiation with KWin, VA import, VPP, VA H.264 encode, rate control, packed headers accepted by iHD | needs your machine | **built but untested** |
| KWin's `pts` is the capture tick on CLOCK_MONOTONIC | KWin source (`addHeader`, `VsyncSource`) | **assumption** until the stage line shows a plausible "capture tick -> daemon" |
| Implicit sync is enough for VA to read KWin's buffer | driver behaviour | **assumption** |
| MSRV 1.80 | `cargo +1.80 check --workspace --all-targets` with a lockfile resolved for 1.80 (`CARGO_RESOLVER_INCOMPATIBLE_RUST_VERSIONS=fallback`) | **tested**. Note: the committed `Cargo.lock` (already on `main`) has `clap_lex 1.1.1`, which needs Rust ≥ 1.85, so the lockfile as committed doesn't build on 1.80. |

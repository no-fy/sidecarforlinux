# Architecture

## Components

```mermaid
flowchart LR
  subgraph iPad["iPad app (Swift, UIKit) - ios-app/"]
    PCV["PencilCaptureView<br/>coalesced touches 240 Hz, hover,<br/>double-tap/squeeze, haptics"]
    VV["VideoView<br/>AVSampleBufferDisplayLayer"]
    subgraph Net["SidecarNet (no UIKit, tested on macOS)"]
      PS["PeerServer<br/>NWListener TLS 1.3<br/>127.0.0.1/::1:47811 (USB)<br/>:47812 + Bonjour (Wi-Fi)"]
      ID["DeviceIdentity<br/>P-256 key in keychain<br/>swift-certificates"]
      VD["VideoFormat / VideoDecoder<br/>VideoToolbox"]
    end
    PCV -- PenBatch / PencilGesture --> PS
    PS -- Haptic --> PCV
    PS -- VideoConfig / VideoFrame --> VV
  end

  subgraph PC["Linux PC - daemon/ (sidecard, Rust)"]
    TR["transport<br/>usbmuxd client / mDNS / TCP"]
    TLS["rustls mTLS + pinning<br/>pairing (commit + 6-digit code)"]
    SES["session<br/>clock sync, latency stats"]
    MAP["PenMapper<br/>tilt, roll, pressure floor,<br/>proximity rules"]
    UI["uinput: Sidecar Apple Pencil<br/>(+ keys device)"]
    VC["videochan<br/>gst-launch subprocess → Annex-B → AVCC"]
    DISP["display<br/>KWin zkde_screencast virtual output"]
  end

  TR == "TLS 1.3 input channel" ==> PS
  VC == "TLS 1.3 video channel" ==> PS
  TR --> TLS --> SES --> MAP --> UI
  UI --> LI["libinput → KWin → Krita"]
  DISP -- PipeWire node --> VC
  SES -. "outputName via D-Bus" .-> KW["KWin InputDevice"]
```

Over USB, `usbmuxd` on the PC tunnels the TCP connection to the iPad's loopback listener, so no IP networking over USB is involved. Over Wi-Fi the daemon finds the iPad with mDNS (`_sidecarlinux._tcp`).

## Session sequence (first pairing)

```mermaid
sequenceDiagram
  participant D as sidecard (PC)
  participant I as iPad app
  D->>I: TCP via usbmux Connect(47811) or Wi-Fi
  D->>I: TLS 1.3 ClientHello (client certificate)
  I-->>D: TLS 1.3 (server certificate, requests client cert)
  D->>I: Hello(channel=input, trusts_peer=false, session_id)
  I-->>D: HelloAck(trusts_peer=false, surface size, caps)
  I-->>D: PairCommit(SHA256(fp_i ‖ N_i))
  D->>I: PairNonce(N_d)
  I-->>D: PairReveal(N_i)
  Note over D,I: both show SAS = H(fp_d ‖ fp_i ‖ N_d ‖ N_i) mod 10^6
  D->>I: PairDecision(yes)
  I-->>D: PairDecision(yes)
  Note over D,I: both pin the peer's fingerprint
  D->>I: MappingInfo, Ping (10 Hz for 1 s, then 1 Hz)
  I-->>D: Pong (clock sync)
  loop while drawing
    I-->>D: PenBatch (coalesced samples + timestamps)
    D->>D: map → uinput write → latency stats
  end
  I-->>D: PencilGesture(double-tap)
  D->>I: Haptic (tool switched)
  opt phase 2
    D->>I: 2nd TLS connection: Hello(channel=video, same session_id)
    D->>I: VideoConfig, VideoFrame…
    I-->>D: VideoFeedback
  end
```

## Where to look

| Concern | Code | Tests |
|---|---|---|
| Wire format | `protocol/rust/src/messages.rs`, `protocol/swift/Sources/SidecarProtocol/Messages.swift` | `protocol/rust/tests/vectors.rs`, `protocol/swift/Tests` (golden `protocol/vectors/vectors.json`) |
| Pairing math | `protocol/*/pairing*` | both of the above |
| Pen → evdev | `daemon/src/tablet.rs`, `gestures.rs` | `daemon/tests/mapper.rs`, `daemon/tests/uinput.rs`, `scripts/ci-uinput-check.sh` |
| Session, TLS, clock | `daemon/src/session.rs`, `tls.rs`, `clock.rs` | `daemon/tests/end_to_end.rs` |
| USB transport | `daemon/src/usbmux.rs`, `transport.rs` | `daemon/tests/usbmux.rs` (fake usbmuxd) |
| iPad networking | `ios-app/Sources/SidecarNet/PeerServer.swift`, `Identity.swift` | `ios-app/Tests/SidecarNetTests` (incl. the real Rust daemon) |
| iPad pencil capture | `ios-app/Sources/SidecarPad/PencilCaptureView.swift` | on-device only (MORNING.md §7) |
| Video | `daemon/src/video.rs`, `videochan.rs`, `display.rs`; `SidecarNet/VideoDecoder.swift`, `SidecarPad/VideoView.swift` | `video_vectors.rs`, `end_to_end.rs` (video), `VideoDecoderTests.swift`, interop test |

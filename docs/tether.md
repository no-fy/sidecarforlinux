# Relationship with Tether

[Tether](https://github.com/zackb/tether) (MIT, v0.3.0 released 2026-10-29) brings iPhone continuity features to Linux: clipboard sync, file transfer, notifications and messages. This project deliberately **doesn't** do any of that. Use Tether alongside it.

## Coexistence

| | Tether | sidecarforlinux |
|---|---|---|
| Purpose | clipboard, files, notifications, messages, calls | Apple Pencil Pro tablet, virtual display |
| Port | 5134 | 47811 (USB, loopback on the iPad), 47812 (Wi-Fi) |
| Bonjour type | `_tether._tcp` | `_sidecarlinux._tcp` |
| Transport | Wi-Fi mTLS, Bluetooth | USB (usbmux) first, Wi-Fi mTLS fallback |
| iOS app | iPhone-focused SwiftUI app | iPad UIKit app |

Nothing collides; both can run at the same time.

## What we took from Tether (ideas only, no code)

Details and code references are in [research/tether-mtls.md](research/tether-mtls.md).

- **Self-signed certificates pinned by SHA-256 fingerprint**, with mutual TLS on every connection.
- **Only pairing messages until pinned:** an unpaired peer can't do anything else.
- **Checking the expected fingerprint on every reconnection.**
- **Where we deliberately differ:**
  - Tether's approval shows a name or a fingerprint prefix. We show a 6-digit code on both screens, derived with a nonce commitment, which resists a man-in-the-middle.
  - We use TLS 1.3 and P-256 v3 certificates rather than TLS 1.2 and RSA (iOS 18.4+ requires the `digitalSignature` key usage).
  - We read the peer certificate per connection from the TLS metadata instead of a shared variable.

## Integration ideas (not implemented)

1. **One pairing for both apps:** both use pinned self-signed certificates. A future `sidecard import-tether` could read Tether's `known_hosts.json`, but the certificates belong to different apps (the iPhone app vs our iPad app), so this would only help if Tether ships an iPad app. Not worth doing now.
2. **Clipboard/file drop into the virtual display:** when phase 2 runs, Tether's iPad/iPhone clipboard sync already covers copy and paste between the iPad and the PC. Nothing to add.
3. **Notifications on the virtual display:** KDE notifications appear on whichever output has focus. Tether's notification forwarding is independent.
4. **Shared tray UI:** both could expose a status over D-Bus (`org.sidecarforlinux.Daemon` for us) so a Plasma widget can show "iPad connected via USB, 4 ms" next to Tether's state. Possible later, low priority.

## Security notes we found in Tether

These are reported here, not upstream. You may want to tell the author privately via their SECURITY.md:
- `TetherServer` stores the client fingerprint in one variable shared by all connections, so a concurrent handshake can be attributed to the wrong peer.
- The iOS share extension doesn't verify that it reached the pinned server.

Both are from reading the source at commit `6477a7f`; neither was reproduced.

# Tether deep dive + mTLS pairing research for an iPad (Swift/xtool) <-> Linux (Rust) sidecar

Research date: 2026-10-30. Everything below was checked against live sources on that date. Tether is
pinned to commit `6477a7f31bce3914fc3c9ac506a2611f36017e42` (HEAD of `main`, "chore: bump version to 0.3.0",
2026-10-29). Line numbers refer to that commit.
`T=https://github.com/zackb/tether/blob/6477a7f31bce3914fc3c9ac506a2611f36017e42`

---

## 0. TL;DR: decisions

| Topic | Decision | Why / source |
|---|---|---|
| Roles | iPad = TLS **server** (`NWListener`), Linux = TLS **client** (rustls) on both transports | Matches the USB constraint (usbmuxd can only dial *into* the device) and needs no inbound firewall port on Linux. Tether needs one (5134/tcp, issue #71). |
| Key type | **ECDSA P-256** on both sides, X.509 **v3**, `KeyUsage{digitalSignature}`, `EKU{serverAuth, clientAuth}`, `BasicConstraints CA:false` | SecKey has no Ed25519 type. iOS 18.4+ Network.framework rejects KU without digitalSignature even inside a verify block. rustls/webpki rejects v1 certs. |
| iOS cert creation | `SecKeyCreateRandomKey` (P-256, permanent, `…ThisDeviceOnly`) + **apple/swift-certificates** `Certificate.PrivateKey(SecKey)`. No hand-built DER. | Tether's hand-built DER broke for non-Latin locales (issue #71). swift-certificates (Apache-2.0, 1.21.1, 2026-10-26) signs with a SecKey directly. |
| SecIdentity | Store key (by tag) and cert (by label) in the keychain, then call **`SecIdentityCreate(nil, cert, key)`**. This is public in the iOS 26+ SDK with availability `ios(11.2)`. Fall back to a `kSecClassIdentity` query on older SDKs. | Apple Security source shows it moved from `SecIdentityPriv.h` to public `SecIdentity.h` in Security-61901, the macOS 26 release. Avoids the "Digital Identities Aren't Real" pitfalls. |
| TLS config (iOS) | `sec_identity_create` → `sec_protocol_options_set_local_identity`; min TLS **1.3**; `sec_protocol_options_set_peer_authentication_required(true)`; `sec_protocol_options_set_verify_block` pins SHA-256(leaf DER); read the per-connection peer cert from `NWConnection.metadata(definition: NWProtocolTLS.definition)` after `.ready` | Servers default to *not* requesting client certs (header). Tether attributes fingerprints through a shared variable, which can race. |
| TLS config (Rust) | `ClientConfig::builder_with_provider(aws_lc_rs).with_protocol_versions(&[&TLS13]).dangerous().with_custom_certificate_verifier(..).with_client_auth_cert(..)`. The verifier **must** still call `rustls::crypto::verify_tls13_signature`. | Without that call nothing proves the peer holds the key. |
| Discovery | `NWListener.service = NWListener.Service(name:type:"_sidecar._tcp", txtRecord:)` plus `NSBonjourServices` and `NSLocalNetworkUsageDescription`. **No multicast entitlement** is needed. | TN3179: the entitlement is only for arbitrary service types or browsing all types. NetService is deprecated as of iOS 27.2 in Apple's docs. |
| USB | Second `NWListener` bound to **127.0.0.1** only, with no Bonjour and no peer-to-peer. The Linux side uses usbmuxd `Connect{DeviceID, PortNumber}` (Rust: `idevice` crate, MIT). | Listening never triggers the Local Network prompt (TN3179), and loopback isn't a "local network". usbmux connections arrive from localhost. DTS calls usbmux "not supported", but it still works on current iOS (Appium uses it for iOS 18+). |
| Free Apple ID | Works. Default keychain access group, no special entitlements. Keychain Sharing and App Groups are also available to free accounts. | Apple "Supported capabilities (iOS)" table. |
| Pairing | Your SAS idea **plus a commit-reveal nonce exchange** inside the TLS session: SAS = H(fp_client‖fp_server‖N_c‖N_s‖TLS-exporter). | Without a commitment, a man-in-the-middle can grind certificates offline until both 6-digit codes match. RFC 6189 (ZRTP) §4.4.1.1 describes this attack. |

---

# Part A: Tether (github.com/zackb/tether)

## A1. Repo facts

- **License:** `LICENSE` is the MIT License text, "Copyright (c) 2026 Zack Bartel". SPDX: **MIT** (GitHub API also reports `MIT`).
- **Created** 2026-04-12. **Last commit** 2026-10-29T01:57:29Z (`6477a7f chore: bump version to 0.3.0`). About 1.5k stars. Not archived.
- **Languages** (GitHub bytes): C++ 2.42 MB, Swift 218 KB, JavaScript 79 KB, Shell 55 KB, CMake 23 KB, Nix 15 KB, plus a few others.
- **Platforms:** Linux desktop (Wayland-first; the pairing dialog falls back to an X11 window), iPhone app (SwiftUI, Xcode project), and a SwiftPM framework with `platforms: [.iOS(.v17), .macOS(.v14)]`. Wi-Fi uses TCP+mTLS. Bluetooth (ANCS/MAP/PBAP/HFP) carries messages, notifications and calls. There is **no USB/usbmux support** (grep for `usbmux|lockdown` finds nothing).
- **Layout:**
  - `apple/Tether/` is the SwiftUI app (`ViewModels/TetherViewModel.swift`, `Views/PairingView.swift`, ...). `apple/Tether.xcodeproj` builds it (Xcode, not xtool).
  - `apple/TetherFramework/` is a SwiftPM package (swift-tools 5.9) shared with the share extension:
    `CertificateManager.swift` (identity and pins), `TetherServer.swift` (NWListener), `TetherConnection.swift` (NWConnection client),
    `BonjourDiscovery.swift`, `TetherProtocol.swift`/`TetherMessage.swift` (newline-delimited JSON), `ShareSender.swift`, `CameraStreamer.swift`, and Tests.
  - `apple/TetherShareExtension/` is the share extension.
  - `src/core/` is the C++ core: `crypto.cpp` (OpenSSL keys, certs, known_hosts), `net.cpp` (TLS server/client and the pairing state machine, about 2.8k lines),
    `discovery.cpp` (Avahi publish/browse), `bluetooth/*`, `wayland*`, `secret_store.cpp`, etc.
  - `src/daemon` (tetherd), `src/cli` (tether), `src/gtk`, `src/tui`, and `src/dialog` (`tether-dialog`, the layer-shell approval prompt).
  - Also `extension/` (browser OTP extension), `docs/` (PROTOCOL.md, DESIGN.md, BLUETOOTH.md ...), `packaging/` (systemd, firewall, flatpak, rpm, container, AppImage), `nix/`, `po/`, `test/`.

## A2. How Tether does pairing and mTLS

### Topology / roles
Every node listens and dials ("Every node is a peer", `docs/PROTOCOL.md` §3). tetherd binds `[::]:5134`. The iPhone runs
`TetherServer` (NWListener on an ephemeral port) **and** `TetherConnection` (NWConnection client). One certificate per node serves
both roles, so the fingerprint is direction-independent.
Source: `$T/docs/PROTOCOL.md` lines 193-212, `$T/src/core/src/net.cpp#L1847-L1916`.

### Certificates and keys

**iOS** (`$T/apple/TetherFramework/Sources/TetherFramework/CertificateManager.swift`):
- **RSA-2048** key via `SecKeyCreateRandomKey`. It is permanent, tagged `net.jeedup.Tether.key`, uses `kSecAttrAccessibleAfterFirstUnlock`, and lives in a shared access group (L250-275).
- The X.509v3 certificate is **hand-built in ASN.1 DER** with no extensions, CN=`Tether-iOS`, 10-year validity and an 8-byte random serial. The TBS is signed with `SecKeyCreateSignature(.rsaSignatureMessagePKCS1v15SHA256)` (L330-391). No swift-certificates and no PKCS#12.
- The cert goes into the keychain with `SecItemAdd(kSecClassCertificate, kSecValueRef, kSecAttrLabel)`, after deleting any stale cert. The SecIdentity is then obtained by **enumerating `kSecClassIdentity` and matching the CN** (L170-193, L295-325):
  ```swift
  // CertificateManager.swift L170-192
  // Identity searches do not reliably filter on key attributes, so enumerate the
  // identities in our access group and match on our certificate's common name.
  let identityQuery: [String: Any] = [
      kSecClass as String: kSecClassIdentity,
      kSecAttrAccessGroup as String: Self.keychainAccessGroup,
      kSecMatchLimit as String: kSecMatchLimitAll,
      kSecReturnRef as String: true,
  ]
  ...
  return identities.first { identity in
      var cert: SecCertificate?
      guard SecIdentityCopyCertificate(identity, &cert) == errSecSuccess, let cert else { return false }
      return (SecCertificateCopySubjectSummary(cert) as String?) == Self.certCommonName
  }
  ```
- The access group is discovered at runtime by adding a probe generic-password item and reading back `kSecAttrAccessGroup` (L35-61). This exists because they share the identity with the share extension (`keychain-access-groups: $(AppIdentifierPrefix)net.jeedup.Tether` plus the App Group `group.net.jeedup.Tether` in `apple/Tether/Tether.entitlements`).
- Fingerprint is SHA-256 over the DER cert, lowercase hex, no separators (L151-155). It is computed the same way as the daemon's `X509_digest(cert, EVP_sha256())` (`crypto.cpp` L288-298).
- History shows the fragility of this approach:
  - Issue #71 "TLS identity not available": first, a failed bootstrap bricked the app permanently (fixed in a522025).
  - Then `failed to create SecCertificate from DER`, because the UTCTime was formatted with locale-aware digits. The fix (86c5e86) forces a Gregorian/UTC/ASCII formatter at L448-457.
  - https://github.com/zackb/tether/issues/71

**Linux** (`$T/src/core/src/crypto.cpp`):
- OpenSSL RSA-2048 via `EVP_PKEY_keygen`. `X509_new()` with serial **1**, CN=`Tether`, 10 years, `X509_sign(..., EVP_sha256())`. There is **no `X509_set_version`**, so this is an **X.509 v1 cert with no extensions** (L121-177).
- Stored as PEM in `$XDG_CONFIG_HOME/tether/{cert.pem,key.pem}`. `key.pem` is chmod 0600 *after* being written (L56-62, L159-175).
- Pins live in `known_hosts.json` (`{fingerprint: name}`), written via a temp file and rename, and cached in memory with mtime revalidation (L179-252).

### TLS parameters

- **TLS 1.2 only on both ends.** The daemon has `SSL_CTX_set_min/max_proto_version(TLS1_2_VERSION)` with the comment "Pin to TLS 1.2 to ensure stability with iOS BoringSSL client" (`crypto.cpp` L73-75, L94-95). iOS does the same (`TetherConnection.swift` L218-220 and `TetherServer.swift` L36-37). No bug is cited for the pin.
- **No ALPN, no SNI pinning, no session resumption logic.**
- Daemon server: `SSL_VERIFY_PEER | SSL_VERIFY_FAIL_IF_NO_PEER_CERT` with a callback that always returns 1. Daemon client: `SSL_VERIFY_PEER` with an accept-all callback. Chain validation is off and identity comes only from fingerprint pinning (`crypto.cpp` L82-101).
- iOS client (`TetherConnection.swift` L212-236). It accepts any server cert and hands the fingerprint to a closure:
  ```swift
  sec_protocol_options_set_local_identity(secOptions, sec_identity_create(identity)!)
  sec_protocol_options_set_min_tls_protocol_version(secOptions, .TLSv12)
  sec_protocol_options_set_max_tls_protocol_version(secOptions, .TLSv12)
  sec_protocol_options_set_verify_block(secOptions, { _, trust, completion in
      let secTrust = sec_trust_copy_ref(trust).takeRetainedValue()
      var fingerprint = ""
      if let certChain = SecTrustCopyCertificateChain(secTrust) as? [SecCertificate],
         let serverCert = certChain.first {
          fingerprint = CertificateManager.fingerprint(ofCertificate: serverCert)
      }
      completion(verify(fingerprint))
  }, .main)
  let params = NWParameters(tls: tlsOptions, tcp: .init())
  params.includePeerToPeer = true
  ```
  `connect(to:identity:)` passes `{ fp in self.serverFingerprint = fp; return true }` (L62-81). Every server is accepted at the TLS layer, and trust is decided afterwards in the view model.
- iOS server (`TetherServer.swift` L27-63). It requires a client certificate, accepts any, and records the fingerprint **in one variable shared by all connections**:
  ```swift
  sec_protocol_options_set_peer_authentication_required(secOptions, true)
  var temporaryHandshakeFingerprint = ""
  sec_protocol_options_set_verify_block(secOptions, { metadata, trust, completion in
      let secTrust = sec_trust_copy_ref(trust).takeRetainedValue()
      if let certChain = SecTrustCopyCertificateChain(secTrust) as? [SecCertificate],
         let clientCert = certChain.first {
          temporaryHandshakeFingerprint = CertificateManager.fingerprint(ofCertificate: clientCert)
      }
      completion(true)
  }, .main)
  ...
  nwListener.newConnectionHandler = { [weak self] connection in
      self?.handleNewConnection(connection, capturedFingerprint: temporaryHandshakeFingerprint) // @autoclosure, read at .ready
  }
  ```
  This is a **TOCTOU/attribution race**. The fingerprint attached to a connection is whatever handshake ran its verify block last before that connection reached `.ready`. If an attacker handshake (any cert, accepted) interleaves with a legitimate paired daemon's handshake, the attacker's connection can be labelled with the paired fingerprint. **Do not copy this pattern.** Read the peer cert per connection from `connection.metadata(definition: NWProtocolTLS.definition)` (see B2). It may be worth reporting privately per Tether's `.github/SECURITY.md`.

### Discovery
- Bonjour type **`_tether._tcp`**. tetherd publishes via Avahi with TXT `fp=<sha256hex>` and `v=1` (`discovery.cpp` L23, L127-129).
- iOS publishes its listener with the deprecated `NetService(domain:"local.", type:"_tether._tcp", name: deviceName, port:)` and TXT `fp` (`TetherServer.swift` L104-115).
- iOS browses with `NetServiceBrowser` (`includesPeerToPeer = true`) and resolves to read the TXT `fp` (`BonjourDiscovery.swift` L57-70, L113-142). The file header says this is because "NWBrowser ignores" TXT records; in fact `NWBrowser.Descriptor.bonjourWithTXTRecord` exists.
- `Info.plist`: `NSBonjourServices = [_tether._tcp]` and an `NSLocalNetworkUsageDescription`. No multicast entitlement.
- The firewall service file opens 5134/tcp and 5353/udp (`packaging/firewall/tether.xml`).

### Pairing protocol (`docs/PROTOCOL.md` §3; `net.cpp` L2169-2370; `TetherViewModel.swift` L567-593, L691-849)
1. TLS handshake with accept-any-cert on both sides. Each side computes the peer fingerprint.
2. If the fingerprint is in `known_hosts`, the session is promoted immediately (`net.cpp` L2196-2200; iOS `handleConnected` L571-575).
3. Otherwise **the dialler sends `{"command":"pair_request","device_name":...}`** ("Dialling is its own consent"). The receiver persists a pending entry, replies `pair_pending`, and prompts its user (`net.cpp` L2330-2365):
   - **Linux prompt:** `tether-dialog` shows "**{device_name} ({first 16 hex chars of fingerprint}...) wants to pair with this device**" with Accept/Reject and a 60 s timeout (`net.cpp` L2654-2701). Alternatives are `tether --accept <fingerprint>` / `tether pending` (CLI, headless).
   - **iPhone (requester) screen:** shows "Waiting for approval on your desktop… Run: tether --accept <its own fingerprint>" and the phone's **own** full fingerprint (`PairingView.swift` L63-79, `TetherViewModel.swift` L728-733).
   - **iPhone (approver, daemon dialled in):** "X wants to pair with this device" with Accept/Reject and **no fingerprint shown** (`PairingView.swift` L64 shows the fingerprint only when `!pairingIsInbound`).
4. The approver sends `pair_accepted`. **Both** sides write the other's fingerprint (`net.cpp` L2288-2296, L2586-2623; iOS `finishPairing` L838-849).
5. Until pinned, each side enforces a direction-aware allowlist. Inbound may send only `pair_request`; outbound may hear only `pair_accepted`, `pair_pending` or `error` (`TetherMessage.swift` L42-47, unit-tested in `PairingPolicyTests.swift`; the daemon mirrors this at `net.cpp` L2282-2370). Anything else from an unpinned peer gets `{"command":"error","message":"unauthorized"}`.

So the user confirms **nothing numeric**. On the approver side, the user can optionally compare a 64-bit fingerprint prefix with the full fingerprint shown on the phone. The requester side never verifies the approver: it pins whatever peer sends `pair_accepted`, which is TOFU from the requester's view.

### Reconnect
- **Daemon:** dials a peer only when mDNS resolves an `fp=` that is already pinned **and** its own fingerprint is lower (`net.cpp` L2023-2029; `src/daemon/main.cpp` L148-159). This avoids duplicate sessions.
- **Expected-fingerprint check:** after a dial made for a fingerprint, the handshake fingerprint must equal the expected one or the connection is dropped. The comment reads "mDNS TXT records are unauthenticated" (`net.cpp` L2182-2193).
- **iPhone:** exponential backoff from 0.7 s to 30 s. It prefers `lastConnectedFingerprint`, and only autoconnects to an unknown host if exactly one is visible. If mDNS finds nothing, it dials the last resolved IP:port, but only if the peer is still pinned (`TetherViewModel.swift` L595-661). On auto-reconnect, a fingerprint mismatch disconnects (L489-494).
- **Camera:** the second TLS connection pins `fingerprint == pinnedFingerprint` inside the verify block (`CameraStreamer.swift` L64-65). This is the correct pattern.
- **Gap:** the share extension (`ShareSender.connectToLastKnownHost`, `ShareSender.swift` L155-226) picks the last host:port and connects with `TetherConnection.connect`, which accepts any server cert. It **never compares `serverFingerprint` with `targetFingerprint`**, so shared content goes to whoever answers at that address.
- Both apps disconnect and stop the listener when backgrounded (`suspendForBackground`, L681-689).

## A3. What to reuse or imitate

The license is MIT, which is compatible with an MIT or Apache-2.0 project. Keep the copyright and permission notice if you copy substantial code.

**Imitate (ideas):**
- Pin the SHA-256 of the DER leaf. Use accept-any-chain plus explicit pinning in the verify callback.
- Use a direction-aware "allowed while unpaired" message allowlist, with unit tests (`PairingPolicyTests.swift`).
- Check the expected fingerprint on every dial driven by mDNS or a cached IP, because TXT is unauthenticated.
- Never mark something paired on a local assertion alone. Wait for the peer's verdict.
- Use exponential reconnect backoff and fall back to the last IP:port when mDNS is unavailable (VPN, AP isolation).
- Keep pending pair requests with a TTL. Make "forget device" remove pins on both ends.
- Pin inside the verify block for secondary connections, as the camera does.

**Reuse with care:** the keychain bootstrap logic in `CertificateManager.generateAndStoreIdentity()`, mainly its handling of half-finished state (reuse an existing key, delete a stale cert). It has to be adapted to P-256 and swift-certificates.

**Don't copy:**
- The hand-built DER. Use swift-certificates.
- RSA-2048 with TLS 1.2-only pinning.
- The shared `temporaryHandshakeFingerprint`.
- The share extension's missing pin check.
- `NetService` (deprecated).
- An X.509 **v1** daemon cert. It would be rejected by rustls/webpki (see B5).
- Prompts that show a name without any verifiable code.

---

# Part B: Network.framework mTLS for a free-team app

## B1. Creating the identity on-device

### Options

| Option | Notes | Verdict |
|---|---|---|
| Hand-built DER (Tether) | Zero dependencies, but you own every ASN.1 detail: INTEGER sign/minimality, UTCTime vs GeneralizedTime, locale. Tether hit a locale bug in production (issue #71). | Avoid |
| **apple/swift-certificates** (`X509`) + swift-crypto/swift-asn1 | Apache-2.0, v1.21.1 (2026-10-26). `Certificate.PrivateKey` has `public init(_ secKey: SecKey) throws` on Darwin, so it signs with a keychain-resident SecKey (EC P-256/384/521 or RSA via `SecKeyWrapper`). X509 types are `@available(iOS 13)`. It pulls in `_CryptoExtras` (BoringSSL C target), which compiles under SwiftPM/xtool but adds build time. | **Use this** |
| PKCS#12 + `SecPKCS12Import` | Apple's "Creating an Identity for Local Network TLS" article uses it for importing an identity made elsewhere. Generating PKCS#12 on-device would need yet another encoder. | Not needed |

Sources:
- `Sources/X509/CertificatePrivateKey.swift` L86-90 and `SecKeyWrapper.swift` in https://github.com/apple/swift-certificates
- Creation example: `Sources/X509/Docs.docc/Creating Certificates.md`

### SecIdentity: `SecIdentityCreate` vs a keychain identity query

- **`SecIdentityCreate(_ allocator: CFAllocator?, _ certificate: SecCertificate, _ privateKey: SecKey) -> SecIdentity?`** is now **public**.
  - Apple docs list availability iOS 11.2+ (https://developer.apple.com/documentation/security/secidentitycreate(_:_:_:)).
  - In Apple's open-source Security project it lives in the public header `keychain/headers/SecIdentity.h` with `API_AVAILABLE(macos(10.12),ios(11.2),...)` as of tag **Security-61901.0.87.0.1**, which is the macOS 26.0 release (`distribution-macOS` tag `macos-260`).
  - In Security-61439.x (macOS 15) it was still only in `SecIdentityPriv.h`.
  - So: **public with the Xcode 26+ SDK**, and back-deployable because the symbol has existed since iOS 11.2. Older forum advice ("remove that private declaration", thread 773016) predates this.
  - Use it when xtool's SDK is Xcode 26 or newer. It returns nil if the key doesn't match the cert.
- **Keychain path** (works with any SDK; Quinn's recommendation in https://developer.apple.com/forums/thread/762996, Aug 2024): add the private key and the certificate, then `SecItemCopyMatching(kSecClass: kSecClassIdentity, kSecReturnRef: true, …)`.
  - From Quinn's "SecItem: Pitfalls and Best Practices" (https://developer.apple.com/forums/thread/724013, rev. 2026-08-31), *Digital Identities Aren't Real*:
    > "The system forms a digital identity by matching the `kSecAttrApplicationLabel` (`klbl`) attribute of the private key with the `kSecAttrPublicKeyHash` (`pkhh`) attribute of the certificate. If you add both items to the keychain and the system doesn't form an identity, check the value of these attributes."
  - Also: adding or removing either half silently adds or removes identities. Filter the identity query (Apple's article uses `kSecAttrLabel`), or enumerate like Tether does.
- **Passing to Network.framework:** `sec_protocol_options_set_local_identity(options, sec_identity_create(identity)!)`. This is identical in Tether and in Apple's article "Creating an Identity for Local Network TLS" (https://developer.apple.com/documentation/network/creating-an-identity-for-local-network-tls).

### Recommended Swift (sketch; adapt error handling)

```swift
import Foundation
import Security
import CryptoKit
import X509
import SwiftASN1

enum DeviceIdentity {
    static let keyTag = Data("org.example.sidecar.identity.key".utf8)
    static let certLabel = "org.example.sidecar.identity.cert"

    static func loadOrCreate() throws -> (SecIdentity, certDER: Data) {
        if let r = try? load() { return r }
        return try create()
    }

    private static func create() throws -> (SecIdentity, certDER: Data) {
        // Clear half-finished state from an interrupted first launch (Tether issue #71 lesson).
        SecItemDelete([kSecClass: kSecClassKey, kSecAttrApplicationTag: keyTag] as CFDictionary)
        SecItemDelete([kSecClass: kSecClassCertificate, kSecAttrLabel: certLabel] as CFDictionary)

        var cfErr: Unmanaged<CFError>?
        guard let key = SecKeyCreateRandomKey([
            kSecAttrKeyType: kSecAttrKeyTypeECSECPrimeRandom,
            kSecAttrKeySizeInBits: 256,
            kSecPrivateKeyAttrs: [
                kSecAttrIsPermanent: true,
                kSecAttrApplicationTag: keyTag,
                // never migrates to backups/new devices: a restored iPad must re-pair
                kSecAttrAccessible: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
            ] as [CFString: Any],
        ] as CFDictionary, &cfErr) else { throw cfErr!.takeRetainedValue() as Error }

        let signer = try Certificate.PrivateKey(key)              // SecKey-backed signer
        let name = try DistinguishedName { CommonName("Sidecar iPad") }
        let now = Date()
        let cert = try Certificate(
            version: .v3,                                         // rustls/webpki require v3
            serialNumber: Certificate.SerialNumber(),             // random, positive, minimal DER
            publicKey: signer.publicKey,
            notValidBefore: now.addingTimeInterval(-86_400),
            notValidAfter: now.addingTimeInterval(20 * 365 * 86_400),
            issuer: name, subject: name,
            signatureAlgorithm: .ecdsaWithSHA256,
            extensions: try Certificate.Extensions {
                Critical(BasicConstraints.notCertificateAuthority)
                Critical(KeyUsage(digitalSignature: true))        // iOS 18.4+ KU rule
                try ExtendedKeyUsage([.serverAuth, .clientAuth])
            },
            issuerPrivateKey: signer)
        var ser = DER.Serializer(); try ser.serialize(cert)
        let der = Data(ser.serializedBytes)

        guard let secCert = SecCertificateCreateWithData(nil, der as CFData) else { throw CocoaError(.coderInvalidValue) }
        let st = SecItemAdd([kSecClass: kSecClassCertificate, kSecValueRef: secCert,
                             kSecAttrLabel: certLabel] as CFDictionary, nil)
        guard st == errSecSuccess || st == errSecDuplicateItem else { throw NSError(domain: NSOSStatusErrorDomain, code: Int(st)) }
        return try load()
    }

    private static func load() throws -> (SecIdentity, certDER: Data) {
        var k: CFTypeRef?, c: CFTypeRef?
        guard SecItemCopyMatching([kSecClass: kSecClassKey, kSecAttrApplicationTag: keyTag,
                                   kSecAttrKeyClass: kSecAttrKeyClassPrivate, kSecReturnRef: true] as CFDictionary, &k) == errSecSuccess,
              SecItemCopyMatching([kSecClass: kSecClassCertificate, kSecAttrLabel: certLabel,
                                   kSecReturnRef: true] as CFDictionary, &c) == errSecSuccess
        else { throw CocoaError(.fileNoSuchFile) }
        let cert = c as! SecCertificate
        // Xcode 26+ SDK: deterministic, no dependence on keychain identity formation.
        guard let id = SecIdentityCreate(nil, cert, k as! SecKey) else { throw CocoaError(.coderInvalidValue) }
        // (Older SDK fallback: SecItemCopyMatching kSecClassIdentity + kSecAttrLabel: certLabel.)
        return (id, SecCertificateCopyData(cert) as Data)
    }

    static func fingerprint(der: Data) -> Data { Data(SHA256.hash(data: der)) }
}
```

Do **not** set `kSecAttrAccessGroup`. Without it, items go into the app's default group (TEAMID.bundleID). With xtool that is the
*prefixed* bundle ID such as `XTL-1234.com.example.Sidecar` (xtool docs `Control.md`). It stays stable as long as the
same Apple ID and team re-sign the app. Signing in with a different Apple ID means a new identity and a re-pair.

## B2. Server side: NWListener requiring and pinning a client cert

```swift
import Network
import Security
import CryptoKit

final class PeerPolicy {            // accessed only on `tlsQueue`
    var pinned: Set<Data> = []      // SHA-256(leaf DER) of paired daemons
    var pairingWindowOpenUntil: Date? = nil
    func allows(_ fp: Data) -> Bool {
        pinned.contains(fp) || (pairingWindowOpenUntil.map { $0 > Date() } ?? false)
    }
}

func makeTLSParams(identity: SecIdentity, policy: PeerPolicy, tlsQueue: DispatchQueue) -> NWParameters {
    let tls = NWProtocolTLS.Options()
    let sec = tls.securityProtocolOptions
    sec_protocol_options_set_local_identity(sec, sec_identity_create(identity)!)
    sec_protocol_options_set_min_tls_protocol_version(sec, .TLSv13)
    sec_protocol_options_set_peer_authentication_required(sec, true)   // servers default to false
    sec_protocol_options_add_tls_application_protocol(sec, "sidecar/1") // optional ALPN
    sec_protocol_options_set_verify_block(sec, { metadata, _trust, complete in
        // Exactly what the peer sent (SecTrust may contain a system-resolved chain).
        var leaf: SecCertificate?
        _ = sec_protocol_metadata_access_peer_certificate_chain(metadata) { c in
            if leaf == nil { leaf = sec_certificate_copy_ref(c).takeRetainedValue() }
        }
        guard let leaf else { complete(false); return }
        let fp = Data(SHA256.hash(data: SecCertificateCopyData(leaf) as Data))
        complete(policy.allows(fp))     // pinned, or user explicitly opened a pairing window
    }, tlsQueue)
    return NWParameters(tls: tls, tcp: .init())
}

// Per-connection identity: read it from the connection, never from shared state set in the verify block.
func peerFingerprint(_ conn: NWConnection) -> Data? {
    guard let md = conn.metadata(definition: NWProtocolTLS.definition) as? NWProtocolTLS.Metadata else { return nil }
    var leaf: SecCertificate?
    _ = sec_protocol_metadata_access_peer_certificate_chain(md.securityProtocolMetadata) { c in
        if leaf == nil { leaf = sec_certificate_copy_ref(c).takeRetainedValue() }
    }
    return leaf.map { Data(SHA256.hash(data: SecCertificateCopyData($0) as Data)) }
}

// TLS exporter (RFC 8446 §7.5) for channel binding in the pairing SAS:
func exporter(_ conn: NWConnection, label: String = "EXPORTER-sidecar-pair-v1") -> Data? {
    guard let md = conn.metadata(definition: NWProtocolTLS.definition) as? NWProtocolTLS.Metadata,
          let dd = label.withCString({ sec_protocol_metadata_create_secret(md.securityProtocolMetadata, strlen($0), $0, 32) })
    else { return nil }
    return Data(dd as DispatchData)
}
```

**Two listeners, one TLS config:**
```swift
// USB (usbmux) listener: loopback only, no Bonjour, no peer-to-peer -> no Local Network prompt
let usbParams = makeTLSParams(identity: id, policy: policy, tlsQueue: q)
usbParams.requiredLocalEndpoint = .hostPort(host: .ipv4(.loopback), port: 47800)
let usbListener = try NWListener(using: usbParams)

// Wi-Fi listener, advertised with Bonjour (triggers the Local Network prompt once)
let wifiListener = try NWListener(using: makeTLSParams(identity: id, policy: policy, tlsQueue: q), on: 47801)
wifiListener.service = NWListener.Service(name: UIDevice.current.name, type: "_sidecar._tcp",
                                          domain: nil, txtRecord: NWTXTRecord(["v": "1", "id": installID]))
```
Untested detail: `requiredLocalEndpoint` on a listener is the documented way to bind a specific address
(NWParameters.requiredLocalEndpoint, iOS 12+). `requiredInterfaceType = .loopback` is an alternative. Verify on a device.

API availability: all `sec_protocol_*` and `sec_*_copy_ref` calls above are `API_AVAILABLE(macos(10.14), ios(12.0))` in Apple's
`protocol/SecProtocolOptions.h`, `SecProtocolMetadata.h` and `SecProtocolTypes.h` (Security-61901.120.67). That includes
`sec_protocol_metadata_access_peer_certificate_chain`, `sec_protocol_metadata_create_secret[_with_context]` and `sec_certificate_copy_ref`.
`sec_protocol_options_set_peer_authentication_optional` is **SPI**, so don't use it.
`NetService` shows "Deprecated iOS 27.2 – Use nw_connection_t or nw_listener_t" in Apple's docs, so use `NWListener.service`.
iOS 26 added the async `NetworkListener`/`NetworkConnection` API. `NWListener` is not deprecated.

### Gotchas (verified)
1. **Peer auth default.** "Enable or disable peer authentication. Clients default to true, whereas servers default to false." (SecProtocolOptions.h)
2. **iOS 18.4 Key Usage check can't be overridden.** Quinn, Apr 2025, https://developer.apple.com/forums/thread/780430:
   > "In 18.4 we added this check to the Network framework path in a way that applies even if you supply your own verification block using `sec_protocol_options_set_verify_block`."

   If a cert has a KeyUsage extension it must include **digitalSignature** (ECDHE). Put `digitalSignature` in both certs. rcgen omits KU by default, which also passes, but be explicit.
3. **SecTrust vs presented chain.** The `SecTrust` passed to the verify block holds the *system-resolved* chain. `sec_protocol_metadata_access_peer_certificate_chain` returns only what the peer sent (Quinn, Mar 2025, https://developer.apple.com/forums/thread/777842). For a self-signed leaf, index 0 is the same either way, but prefer the metadata API.
4. **Don't attribute fingerprints via shared state** (Tether race, A2). The verify block's `metadata` can't be mapped to an `NWConnection`, so re-read the peer cert from `conn.metadata(...)` after `.ready`.
5. **Key types.** SecKey asymmetric types are RSA and EC only (`kSecAttrKeyTypeRSA`, `kSecAttrKeyTypeECSECPrimeRandom` in `SecItem.h`; there is no Ed25519). So a SecIdentity can't be Ed25519. **Use P-256 ECDSA.** It avoids RSA-PSS requirements in TLS 1.3 and is supported by rustls (`ECDSA_NISTP256_SHA256`) in both directions. Don't rely on Ed25519 peer certs on Apple's side either.
6. **TLS 1.3 client-auth timing.** The client sends its cert after the server's Finished. If the iPad's verify block rejects it, rustls has usually already returned from `connect()` and only sees an alert on the first read. **Have the server speak first** (e.g. a `hello` frame) so the client knows it was accepted. Avoid post-handshake auth. The forum report https://developer.apple.com/forums/thread/694273 (URLSession plus a TLS 1.3 server) is unresolved and not relevant if the cert is requested in the main handshake, as `peer_authentication_required` does.
7. **TLS version.** Tether pins 1.2 on both ends "for stability" and cites no bug. I found no Apple-documented problem with TLS 1.3 plus client certs on NWListener. Plan TLS 1.3-only, test it early on your minimum iPadOS, and keep a config switch.
8. **iOS 26 post-quantum key exchange.** iOS 26 turns on X25519MLKEM768 by default for URLSession and Network APIs (Apple security guide; support article https://support.apple.com/en-gb/122756 warns about "large ClientHello" breakage in legacy servers). rustls 0.23 (default features `aws_lc_rs`, `prefer-post-quantum`) supports X25519MLKEM768, so there is nothing to do. With the `ring` provider, rustls negotiates X25519 instead.
9. **CertificateVerify.** BoringSSL's custom-verify hook replaces *certificate* verification only (`SSL_CTX_set_custom_verify` doc in boringssl `ssl.h`). The handshake signature is still checked by the state machine. Apple's fork isn't public, so **add a negative test**: present cert A while signing with key B; it must fail.
10. **Apple's sample** in "Creating an Identity for Local Network TLS" calls `completionHandler(false)` and then falls through to `completionHandler(true)`. Always `return` after calling the completion.
11. **iOS 26 "verify block never called"** (https://developer.apple.com/forums/thread/803127, Oct 2025). Quinn's hypothesis is a peer that drops the connection on a newer ClientHello. It isn't relevant when you control both ends, but it is a reason to keep rustls current.

## B3. Entitlements and a free Apple ID personal team

Apple's capability table (https://developer.apple.com/help/account/reference/supported-capabilities-ios, column "Apple Developer" = free) lists these for free accounts:
**App groups, Background modes, Data protection, HealthKit, HomeKit, Inter-App Audio, Keychain sharing, Maps, Wireless Accessory Configuration.**
Network extensions, Multipath, Access WiFi info, Push and others are paid-only.
**Multicast networking isn't in the self-service table at all.** It is a request-only managed entitlement, so treat it as unavailable.

What this project needs:
- **Keychain:** nothing. The default access group (application-identifier) is always present. Keychain Sharing/App Groups are free too, if you later add an extension.
- **TLS (Network.framework):** no entitlement.
- **Bonjour advertise via `NWListener.service`:** no entitlement. You need the `NSBonjourServices` (`_sidecar._tcp`) and `NSLocalNetworkUsageDescription` Info.plist keys (TN3179, https://developer.apple.com/documentation/technotes/tn3179-understanding-local-network-privacy). TN3179's multicast table:
  > "Some uncommon Bonjour operations also require the multicast entitlement. | Working with arbitrary Bonjour service types | yes | Browsing for all advertised service types | yes"

  Registering a fixed, declared type is neither. TN3179 also says: "Registering a service with Bonjour | yes" for *local network access*, so the user sees the Local Network alert once.
- **xtool:** add plist keys via `infoPath:` and entitlements via `entitlementsPath:` in `xtool.yml`. xtool warns that some entitlements (e.g. Network Extension) "don't work with free developer accounts" and prefixes bundle IDs for free accounts (https://github.com/xtool-org/xtool `Documentation/xtool.docc/Control.md`).

## B4. Loopback listener, Local Network prompt, and usbmux

- **No prompt for listening.** TN3179's table: "Listening for and accepting incoming TCP connections | **no**" (local network access *not* required). "A local network is an IP network associated with a broadcast-capable network interface" (Wi-Fi, Ethernet; not loopback). A loopback-only `NWListener` with no `service` and `includePeerToPeer = false` should **not** trigger the alert. The Wi-Fi listener's Bonjour registration does. Accepting the daemon's Wi-Fi connection doesn't need the permission either, so manual-IP pairing still works if the user denies it.
  - Avoid `includePeerToPeer` (Tether sets it). TN3179 notes the alert can appear "off Wi-Fi" when you "perform a Bonjour operation with peer-to-peer Wi-Fi enabled".
- **How usbmux connections arrive.** From the usbmuxd README (v1.0.8, https://github.com/libimobiledevice/usbmuxd/blob/v1.0.8/README L133-136):
  > "6. usbmuxd sends a pseudo-TCP SYN packet to the phone / 7. the phone's kernel driver receives the SYN packet and itself opens a TCP connection to localhost on the afc port"

  The current README says "you can connect to any listening localhost socket on the device." So the app sees a normal TCP connection from **127.0.0.1**.
- **PeerTalk confirms the loopback bind** (https://github.com/rsms/peertalk, MIT, last push 2022-04-27):
  - `Peertalk iOS Example/PTViewController.m` L34: `[channel listenOnPort:PTExampleProtocolIPv4PortNumber IPv4Address:INADDR_LOOPBACK ...]`
  - `Peertalk Swift Example/ViewController.swift` L32: `channel.listen(on: ..., IPv4Address: INADDR_LOOPBACK)`
  - Host side, `peertalk/PTUSBHub.m` L225-240, sends plist `{MessageType: "Connect", DeviceID, PortNumber}` with the port byte-swapped (`((port<<8)&0xFF00)|(port>>8)`).
- **Still works on recent iOS (indirect evidence):**
  - Appium xcuitest-driver master (2026-10-29), `lib/device/device-connections-factory.ts` L244-247: "Forwarding may use `appium-ios-remotexpc` on iOS 18+ (usbmux or tunnel) with fallback to `appium-ios-device` connectPort". WebDriverAgent listens on the device and is reached over usbmux today.
  - The Rust crate **`idevice`** (jkcoxson/idevice, MIT, 0.1.68, pushed 2026-10-27) implements `UsbmuxdConnection::{get_devices, listen (Attached/Detached), connect_to_device(device_id, port, label)}`. `Idevice::get_socket()` returns the raw stream to wrap in tokio-rustls. The protocol is small enough to reimplement (~150 lines of plist over `/var/run/usbmuxd`).
  - Note that **usbmuxd itself is GPL-2.0**. You only talk to its socket, which doesn't create a license dependency.
- **Apple's stance:** Quinn (Aug 2020, https://developer.apple.com/forums/thread/658393): "This technique is not supported and I strongly recommend that you avoid it." It works but is unsupported, so keep Wi-Fi as a first-class path. In May 2023 he observed Bonjour working over a USB cable to a Mac with all radios off ("may only work because of developer bits").
- **Trust prompt:** Linux usbmuxd runs a lockdown "preflight". It triggers the iPad's "Trust This Computer" dialog for unpaired hosts but makes the device visible anyway (`src/preflight.c`, "make device visible anyways"). **Unverified:** whether app ports accept usbmux `Connect` before the host is trusted. Test this, and treat `ConnectionRefused` (reply code 3) as "app not listening or not trusted".
- **Security implication:** loopback is not a trust boundary. Any app on the iPad can connect to 127.0.0.1:port, and on Linux any local user can usually reach `/var/run/usbmuxd`. **Use the same mTLS and pinning on USB as on Wi-Fi.**

## B5. Rust side (rustls 0.23.46, rcgen 0.14.10, tokio-rustls 0.26.6)

Crate versions (crates.io, 2026-10-30): rustls 0.23.46 (Apache-2.0 OR ISC OR MIT), rcgen 0.14.10, tokio-rustls 0.26.6,
sha2 0.11.1, mdns-sd 0.21.6 (Apache-2.0 OR MIT), idevice 0.1.68 (MIT).

```rust
// Cargo: rustls = "0.23", tokio-rustls = "0.26", rcgen = { version = "0.14", default-features = false, features = ["aws_lc_rs", "pem"] },
//        sha2 = "0.11", time = "0.3"
use std::sync::Arc;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::{CertificateError, DigitallySignedStruct, Error, SignatureScheme};
use sha2::{Digest, Sha256};

// --- long-lived identity (store cert.der + key.pk8 (0600) under $XDG_CONFIG_HOME/sidecar/) ---
fn make_identity() -> Result<(CertificateDer<'static>, PrivateKeyDer<'static>), rcgen::Error> {
    use rcgen::*;
    let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)?;
    let mut p = CertificateParams::new(Vec::<String>::new())?;      // no SAN needed
    p.distinguished_name = { let mut dn = DistinguishedName::new(); dn.push(DnType::CommonName, "sidecar-linux"); dn };
    p.not_before = time::OffsetDateTime::now_utc() - time::Duration::days(1);
    p.not_after  = p.not_before + time::Duration::days(365 * 20);   // rcgen default is year 4096
    p.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    p.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth, ExtendedKeyUsagePurpose::ClientAuth];
    let cert = p.self_signed(&key)?;                                 // always X.509 v3
    Ok((cert.der().clone(), PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()))))
}

#[derive(Debug)]
pub enum PinMode { Pairing, Pinned(Vec<[u8; 32]>) }

#[derive(Debug)]
pub struct PinningVerifier { pub mode: PinMode, pub provider: Arc<CryptoProvider> }

impl ServerCertVerifier for PinningVerifier {
    fn verify_server_cert(&self, end_entity: &CertificateDer<'_>, _i: &[CertificateDer<'_>],
                          _sn: &ServerName<'_>, _ocsp: &[u8], _now: UnixTime) -> Result<ServerCertVerified, Error> {
        let fp: [u8; 32] = Sha256::digest(end_entity.as_ref()).into();
        match &self.mode {
            PinMode::Pairing => Ok(ServerCertVerified::assertion()),           // accept + record (read later)
            PinMode::Pinned(p) if p.contains(&fp) => Ok(ServerCertVerified::assertion()),
            _ => Err(Error::InvalidCertificate(CertificateError::ApplicationVerificationFailure)),
        }
    }
    // MUST verify CertificateVerify: this is the proof of key possession.
    fn verify_tls12_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct)
        -> Result<HandshakeSignatureValid, Error> { verify_tls12_signature(m, c, d, &self.provider.signature_verification_algorithms) }
    fn verify_tls13_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct)
        -> Result<HandshakeSignatureValid, Error> { verify_tls13_signature(m, c, d, &self.provider.signature_verification_algorithms) }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

fn client_config(mode: PinMode, cert: CertificateDer<'static>, key: PrivateKeyDer<'static>)
    -> Result<rustls::ClientConfig, Error> {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let mut cfg = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinningVerifier { mode, provider }))
        .with_client_auth_cert(vec![cert], key)?;
    cfg.alpn_protocols = vec![b"sidecar/1".to_vec()];
    Ok(cfg)
}
// After `TlsConnector::from(Arc::new(cfg)).connect(ServerName::try_from("sidecar.invalid")?.to_owned(), stream)`:
//   let (_, conn) = tls.get_ref();
//   let fp = Sha256::digest(conn.peer_certificates().unwrap()[0].as_ref());
//   let mut ekm = [0u8; 32]; conn.export_keying_material(&mut ekm, b"EXPORTER-sidecar-pair-v1", None)?;
```

API checks against rustls `v/0.23.46` source:
- `ServerCertVerifier` lives at `rustls::client::danger` with `verify_server_cert`, `verify_tls12_signature`, `verify_tls13_signature` and `supported_verify_schemes`.
- `rustls::crypto::{verify_tls12_signature, verify_tls13_signature}` are re-exported from `webpki`.
- Also present: `CertificateError::ApplicationVerificationFailure`, `ClientConfig::builder_with_provider`, `.with_protocol_versions`, `.dangerous().with_custom_certificate_verifier`, `.with_client_auth_cert`, `CommonState::peer_certificates`, `export_keying_material(output, label, Option<&[u8]>)`.
- rcgen `v0.14.10`: `KeyPair::generate_for`, `CertificateParams::{new, self_signed}`, `key_usages`, `extended_key_usages`. The default `not_after` is **4096-01-01** and default features are `["crypto","pem","ring"]`.

Interop gotchas:
- **rustls only accepts X.509 v3 even in the custom-verifier path.** `verify_tls13_signature` does `webpki::EndEntityCert::try_from(cert)`, and webpki's `version3()` returns `Error::UnsupportedCertVersion` for anything else (rustls/webpki `src/cert.rs` L260-276). A Tether-style OpenSSL v1 cert would fail. swift-certificates and rcgen both emit v3.
- **P-256 ECDSA with SHA-256** is the safest scheme on both stacks. Don't use Ed25519 (no SecKey type). RSA would need PSS in TLS 1.3.
- **TLS 1.3 exporter:** "providing no context computes the same value as providing an empty context" (RFC 8446 §7.5). rustls `None` therefore equals Apple's `sec_protocol_metadata_create_secret` (no context). Use a label starting with `EXPORTER-`.
- **SNI:** connect with a dummy DNS name or an IP `ServerName`. The iPad ignores it.
- **Crypto backends:** rustls defaults to aws-lc-rs and rcgen defaults to ring. Pick one backend (features above) to avoid shipping both.
- **Discovery on Linux:** browse `_sidecar._tcp` via Avahi (D-Bus) when `avahi-daemon` runs, as Tether does with libavahi-client, or with the pure-Rust `mdns-sd` crate. Treat TXT as a hint only. Tether's rule is "mDNS TXT records are unauthenticated".

---

# Part C: Pairing protocol

## C1. Your proposal and its main weakness

Your proposal:
- Long-lived self-signed P-256 certs on both sides.
- The first connection accepts any cert and records it.
- Both sides display SAS = 6 digits of SHA-256(sorted(fp_a, fp_b)).
- The user confirms on both sides, then both pin, and unknown certs are rejected afterwards.

**Weakness: the SAS is computable before the attacker commits to anything, so a MITM can grind it.**
- Both honest certs are static and public.
  - The iPad's leaf is sent to *anyone* who opens a TLS connection, because the server cert goes out before client auth. It may also be in TXT.
  - The daemon's cert is revealed to any server it dials, e.g. a rogue `_sidecar._tcp` advertiser.
- A MITM sits as the iPad toward the daemon (cert M1) and as the daemon toward the iPad (cert M2). The daemon then shows SAS(A, M1) and the iPad shows SAS(M2, B).
- The attacker fixes M1, computes the target t = SAS(A, M1), and varies bytes of M2 (serial number, or any field; nobody checks the self-signature) until SAS(M2, B) = t.
- 6 digits is about 20 bits, so that is about 10^6 SHA-256 evaluations: **well under a second, offline, before pairing starts**.

Sorting the fingerprints doesn't change this. RFC 6189 (ZRTP) §4.4.1.1 describes exactly this:
> "Without this hash commitment feature, a MiTM attacker would acquire both the pvi and pvr public values from the two parties before having to choose his own two DH public values for his MiTM attack. He could then use that information to quickly perform a bunch of trial DH calculations for both sides until he finds two with a matching SAS. … A hash commitment precludes this attack by forcing the MiTM to choose his own two DH public values before learning the public values of either of the two parties."

Bluetooth SSP Numeric Comparison uses the same commit-then-reveal-nonce pattern for the same reason.

Mixing only the TLS exporter into the SAS is **not enough** either. As the TLS server on the daemon-facing leg, the MITM picks ServerHello.random after seeing the ClientHello and can grind it (about 10^6 HKDF/transcript computations, which is seconds) to hit the SAS from the other, already finished leg. Fresh committed nonces are what fix this.

Secondary issues:
- **"Accept any cert" must be gated.** Only accept unknown certs while the user has explicitly opened a pairing window (iPad: a "Pair new computer" button with a 2-minute timeout; daemon: `sidecar pair`). Otherwise any LAN host can start prompts. Tether's PROTOCOL.md admits an attacker "can open a connection and sit at the pairing prompt".
- **Pin the cert from *this* TLS session**, never a TXT `fp`.
- **One-shot property.** Even with commitments, a MITM wins with probability 10^-6 per attempt. Abort on mismatch, show a warning, and rate-limit new pairing windows. Users who tap "Match" without comparing defeat any SAS. Make the code large on both screens and require an explicit tap on each device.
- **Role binding.** Use a fixed order (client fp, then server fp) rather than sorting. Roles are fixed in this design, and it removes any ambiguity.

## C2. Revised protocol ("Sidecar Pairing v1")

Roles are fixed: **D** = Linux daemon (TLS client), **P** = iPad (TLS server). Each has a long-lived P-256 identity
(B1/B5). Pins are a set of SHA-256(leaf DER) values. On the iPad, keep them in a keychain generic-password item (or a file in Application Support). On Linux, use `known_peers.json`.

```
Preconditions: user taps "Pair" on P (opens window W, ~120 s) and runs/clicks "Pair" on D.

1. D --TCP (usbmux 127.0.0.1:47800 or Wi-Fi Bonjour)--> P
   TLS 1.3, ALPN "sidecar/1", mutual certs.
   P verify block: accept fp_D iff fp_D ∈ pins OR W open.
   D verifier:     PinMode::Pairing (accept, but CertificateVerify still checked).
2. Both compute fp_D = SHA256(D.leaf), fp_P = SHA256(P.leaf) from THIS session,
   E = TLS-Exporter("EXPORTER-sidecar-pair-v1", "", 32).
3. P -> D : PAIR_HELLO { v:1, name_P, commit = SHA256("sidecar-commit-v1" ‖ N_P) }     (N_P: 32 random bytes)
4. D -> P : PAIR_NONCE { name_D, N_D }                                                  (N_D: 32 random bytes)
5. P -> D : PAIR_REVEAL { N_P }        D checks commit; mismatch => abort + warn.
6. Both:   h   = SHA256("sidecar-sas-v1" ‖ fp_D ‖ fp_P ‖ N_D ‖ N_P ‖ E)
           SAS = (big-endian u64 of h[0..8]) mod 1_000_000, shown as "123 456".
7. Each user compares and taps Match / No match on their own device.
   Each side sends PAIR_CONFIRM{ok:bool}. A side pins the peer's fp only when
   (local ok) AND (peer ok) are both true; otherwise neither pins. Close window W.
8. Close the connection; reconnect in strict mode (P: pins only; D: PinMode::Pinned).
```

Why the order matters: P reveals its nonce last, so P commits first. In both orderings of the two MITM legs, the
attacker must fix its own inputs on each leg before the honest peer's fresh nonce on that leg is known. Each attempt therefore succeeds with probability 10^-6.

**Steady state:**
- P's verify block: `complete(pins.contains(fp))`.
- D: `PinMode::Pinned(vec![expected])`. Over USB, map the usbmux UDID to a fingerprint. Over Wi-Fi, try candidate instances and require the fingerprint to match.
- After `.ready`, P re-derives the fingerprint from `conn.metadata` and **sends the first frame** (`HELLO{features}`). D treats "no HELLO within N s" as rejected.
- Before pinning, mirror Tether's allowlist: only `PAIR_*` frames in pairing mode; reject everything else.

**Forget:** remove the pin locally. If connected, send `UNPAIR` so the peer drops its pin too. This mirrors Tether's `forget_device`, where "Both ends must forget".

**Key loss / new install:** the fingerprint changes and the peer rejects it at TLS, so the user re-pairs. Pin the cert hash (simple; 20-year validity). Pin SPKI-SHA256 instead if you ever want to re-issue certs with the same key.

**Storage:**
- iPad key: `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly`.
- Linux key: write with mode 0600 *at creation* (`OpenOptions::mode(0o600)`), not chmod afterwards as Tether does.

## C3. How Tether handles the same problems

| Concern | Tether | Proposed |
|---|---|---|
| What the user verifies | Desktop dialog shows name plus a **64-bit fingerprint prefix** (`net.cpp` L2670-2675). The phone shows its own full fingerprint. Comparing is optional. The inbound prompt on the phone shows **name only**. | 6-digit SAS with commitment on **both** screens, mandatory on both |
| MITM resistance | Only if the approving user actually compares the 16-hex prefix (grinding a 64-bit prefix is infeasible). The **requester pins whoever sends `pair_accepted`**, which is TOFU, so a rogue advertiser the phone dials can just send `pair_accepted`. | SAS-MCA bound, 10^-6 per attempt |
| Who can trigger prompts | Any LAN host (documented) | Only during a user-opened window |
| Proof of key possession | TLS CertificateVerify (documented in PROTOCOL.md) | Same, and the rustls verifier must call `verify_tls13_signature` |
| Fingerprint per connection | Shared variable in `TetherServer` (racy) | `conn.metadata` after `.ready` |
| Reconnect pinning | mDNS fp hint plus a handshake match check on the daemon; auto-reconnect check on iOS; **share extension doesn't check** | Strict pin in the TLS verifier on every connection |
| TLS | 1.2 only, RSA-2048, v1 daemon cert | 1.3, P-256, v3 certs with KU/EKU |

## C4. Test checklist
- rustls ↔ NWListener TLS 1.3 handshake with P-256 on your minimum iPadOS and on iPadOS 26 (PQ key share).
- Negative tests:
  - Unpinned client rejected at TLS in strict mode.
  - Wrong-key CertificateVerify fails.
  - A cert without digitalSignature KU fails (confirms the 18.4 behavior).
  - Two concurrent handshakes are attributed correctly.
- Loopback listener: confirm **no** Local Network alert on a fresh install. Confirm usbmux `Connect` works before and after "Trust This Computer".
- Bonjour advertisement with `NSBonjourServices`: the alert appears once. Denying it still allows manual-IP connections.
- Force-quit during first-launch identity creation, then relaunch. Bootstrap must recover (Tether a522025/86c5e86).
- Non-Latin device locale and a non-Gregorian calendar (Tether issue #71).

---

## Sources (all fetched 2026-10-30)

**Tether (MIT):**
- https://github.com/zackb/tether (commit 6477a7f)
- `apple/TetherFramework/Sources/TetherFramework/{CertificateManager,TetherServer,TetherConnection,BonjourDiscovery,TetherMessage,ShareSender,CameraStreamer}.swift`
- `apple/Tether/ViewModels/TetherViewModel.swift`, `apple/Tether/Views/PairingView.swift`
- `src/core/src/{crypto,net,discovery}.cpp`, `src/daemon/main.cpp`
- `docs/PROTOCOL.md`, `.github/SECURITY.md`
- Issues #71 and #104

**Apple:**
- TN3179 Understanding local network privacy (rev. 2026-10-06): https://developer.apple.com/documentation/technotes/tn3179-understanding-local-network-privacy
- Creating an Identity for Local Network TLS: https://developer.apple.com/documentation/network/creating-an-identity-for-local-network-tls
- SecIdentityCreate: https://developer.apple.com/documentation/security/secidentitycreate(_:_:_:)
- Supported capabilities (iOS): https://developer.apple.com/help/account/reference/supported-capabilities-ios
- Apple Security source:
  - https://github.com/apple-oss-distributions/Security (tags Security-61901.120.67, Security-61901.0.87.0.1, Security-61439.140.12)
  - Files: `keychain/headers/SecIdentity.h`, `SecIdentityPriv.h`, `SecItem.h`, `protocol/SecProtocolOptions.h`, `SecProtocolMetadata.h`, `SecProtocolTypes.h`
  - Release mapping: https://github.com/apple-oss-distributions/distribution-macOS (macos-260 → Security-61901.0.87.0.1)
- Forums (Quinn / DTS):
  - 724013 SecItem Pitfalls
  - 762996 and 773016 (SecIdentity on iOS)
  - 777842 (peer chain vs SecTrust)
  - 780430 (iOS 18.4 KU)
  - 803127 (iOS 26 TLS)
  - 658393 (usbmux "not supported")
  - 703234 TLS For Accessory Developers
  - 694273
- Post-quantum TLS: https://support.apple.com/en-gb/122756
- NetService deprecation: https://developer.apple.com/documentation/foundation/netservice

**Rust:**
- rustls v/0.23.46 source (`rustls/src/verify.rs`, `webpki/verify.rs`, `client/builder.rs`, `crypto/aws_lc_rs/mod.rs`, `Cargo.toml`)
- rustls/webpki `src/cert.rs`
- rcgen v0.14.10 source
- crates.io API for versions
- idevice: https://github.com/jkcoxson/idevice (`idevice/src/usbmuxd/mod.rs`)

**USB:**
- usbmuxd README (current and v1.0.8): https://github.com/libimobiledevice/usbmuxd
- `src/preflight.c`
- libusbmuxd README (iproxy)
- PeerTalk: https://github.com/rsms/peertalk
- Appium xcuitest-driver `lib/device/device-connections-factory.ts`

**Other:**
- swift-certificates: https://github.com/apple/swift-certificates (Package.swift, CertificatePrivateKey.swift, SecKeyWrapper.swift, Docs.docc)
- xtool: https://github.com/xtool-org/xtool (Documentation/xtool.docc/Control.md)
- RFC 6189 §4.4.1.1: https://www.rfc-editor.org/rfc/rfc6189
- RFC 8446 §7.5: https://www.rfc-editor.org/rfc/rfc8446
- BoringSSL `include/openssl/ssl.h` (`SSL_CTX_set_custom_verify`)

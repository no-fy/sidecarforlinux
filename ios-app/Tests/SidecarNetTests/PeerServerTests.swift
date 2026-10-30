// Runs the iPad app's networking layer (PeerServer + keychain identity +
// pairing state machine) on macOS against a Network.framework TLS client
// that plays the Linux daemon's role. Apple's TLS stack, client
// certificates, commitment/SAS and session hand-over are exercised for real.
import Foundation
import Network
import Security
import SidecarProtocol
import XCTest

@testable import SidecarNet

@MainActor
final class RecordingDelegate: PeerServerDelegate {
    var statuses: [PeerServer.Status] = []
    var pairingCodes: [String] = []
    var pairingResults: [Bool] = []
    var haptics: [Haptic] = []
    var acceptPairing = true

    func serverStatusChanged(_ status: PeerServer.Status) { statuses.append(status) }
    func pairingRequested(code: String, peerName: String, peerFingerprint: String, decide: @escaping (Bool) -> Void) {
        pairingCodes.append(code)
        decide(acceptPairing)
    }
    func pairingFinished(accepted: Bool, peerName: String) { pairingResults.append(accepted) }
    func hapticRequested(_ haptic: Haptic) { haptics.append(haptic) }
    func latencyReported(_ report: LatencyReport) {}
    func mappingReported(_ mapping: MappingInfo) {}

    var isConnected: Bool {
        if case .connected = statuses.last { return true }
        return false
    }
}

/// Minimal daemon stand-in: TLS client with its own certificate.
final class TestDaemon: @unchecked Sendable {
    let connection: NWConnection
    let queue = DispatchQueue(label: "test.daemon")
    private let lock = NSLock()
    private var decoder = FrameDecoder()
    private var inbox: [Message] = []
    private var ready = false
    private var closed = false

    init(identity: DeviceIdentity, port: UInt16) {
        let params = makeTLSParameters(identity: identity.identity, queue: queue) { _ in true }
        connection = NWConnection(host: "127.0.0.1", port: NWEndpoint.Port(rawValue: port)!, using: params)
        connection.stateUpdateHandler = { [weak self] st in
            guard let self else { return }
            self.lock.lock()
            switch st {
            case .ready: self.ready = true
            case .failed, .cancelled: self.closed = true
            default: break
            }
            self.lock.unlock()
        }
        connection.start(queue: queue)
        receive()
    }

    private func receive() {
        connection.receive(minimumIncompleteLength: 1, maximumLength: 65536) { [weak self] data, _, done, error in
            guard let self else { return }
            self.lock.lock()
            if let data, !data.isEmpty {
                self.decoder.push([UInt8](data))
                while let f = try? self.decoder.nextFrame(), let m = try? Message.decode(type: f.type, payload: f.payload) {
                    self.inbox.append(m)
                }
            }
            if done || error != nil { self.closed = true }
            let stop = self.closed
            self.lock.unlock()
            if !stop { self.receive() }
        }
    }

    var isReady: Bool { lock.lock(); defer { lock.unlock() }; return ready }
    var isClosed: Bool { lock.lock(); defer { lock.unlock() }; return closed }

    func send(_ m: Message) {
        connection.send(content: Data(m.encode()), completion: .contentProcessed { _ in })
    }

    /// Next message matching `pred`, skipping others (pings/reports).
    func next(timeout: TimeInterval = 5, where pred: (Message) -> Bool = { _ in true }) async throws -> Message {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            lock.lock()
            if let i = inbox.firstIndex(where: pred) {
                let m = inbox.remove(at: i)
                lock.unlock()
                return m
            }
            lock.unlock()
            try await Task.sleep(nanoseconds: 10_000_000)
        }
        throw NSError(domain: "TestDaemon", code: 1, userInfo: [NSLocalizedDescriptionKey: "timeout waiting for message"])
    }

    func cancel() { connection.cancel() }
}

final class PeerServerTests: XCTestCase {
    var ipadNS = DeviceIdentity.Namespace("test-ipad-\(UUID().uuidString.prefix(8))", commonName: "Test iPad")
    var daemonNS = DeviceIdentity.Namespace("test-daemon-\(UUID().uuidString.prefix(8))", commonName: "Test daemon")
    var otherNS = DeviceIdentity.Namespace("test-other-\(UUID().uuidString.prefix(8))", commonName: "Other daemon")

    override func tearDown() {
        for ns in [ipadNS, daemonNS, otherNS] { try? DeviceIdentity.deleteAll(ns) }
    }

    func waitUntil(_ timeout: TimeInterval = 5, _ cond: @escaping @MainActor () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if await MainActor.run(body: cond) { return }
            try await Task.sleep(nanoseconds: 10_000_000)
        }
        XCTFail("condition not met within \(timeout) s")
    }

    func hello(trusts: Bool, name: String = "test-pc") -> Message {
        .hello(Hello(channel: .input, trustsPeer: trusts, caps: Caps.gestures | Caps.haptics, sessionID: 42, name: name))
    }

    func testIdentityIsCreatedOnceAndIsAValidV3Certificate() throws {
        let a = try DeviceIdentity.loadOrCreate(ipadNS)
        let b = try DeviceIdentity.loadOrCreate(ipadNS)
        XCTAssertEqual(a.certificateDER, b.certificateDER, "identity must persist")
        XCTAssertNotNil(SecCertificateCreateWithData(nil, Data(a.certificateDER) as CFData))
        var key: SecKey?
        XCTAssertEqual(SecIdentityCopyPrivateKey(a.identity, &key), errSecSuccess)
        XCTAssertNotNil(key)
    }

    func testPairingThenSessionThenReconnectWithoutPairing() async throws {
        let ipad = try DeviceIdentity.loadOrCreate(ipadNS)
        let daemonID = try DeviceIdentity.loadOrCreate(daemonNS)
        let trust = TrustStore(defaultsKey: "test.trust.\(UUID().uuidString)")
        let port = UInt16.random(in: 42000...48000)
        let server = PeerServer(identity: ipad, trust: trust, usbPort: port)
        let delegate = await MainActor.run { RecordingDelegate() }
        await MainActor.run { server.delegate = delegate }
        server.start(wifi: false)
        try await waitUntil { delegate.statuses.contains { if case .waiting = $0 { return true } else { return false } } }

        // --- first connection: pairing
        let d = TestDaemon(identity: daemonID, port: port)
        try await waitUntilReady(d)
        let serverDER = try XCTUnwrap(peerCertificateDER(of: d.connection), "server certificate")
        XCTAssertEqual(Pairing.fingerprint(certificateDER: serverDER), ipad.fingerprint)

        d.send(hello(trusts: false))
        guard case .helloAck(let ack) = try await d.next() else { return XCTFail("expected HelloAck") }
        XCTAssertFalse(ack.trustsPeer)
        XCTAssertEqual(ack.major, SidecarProtocol.major)
        guard case .pairCommit(let commit) = try await d.next() else { return XCTFail("expected PairCommit") }
        var nonceD = [UInt8](repeating: 0, count: 32)
        _ = SecRandomCopyBytes(kSecRandomDefault, 32, &nonceD)
        d.send(.pairNonce(nonceD))
        guard case .pairReveal(let nonceI) = try await d.next() else { return XCTFail("expected PairReveal") }
        XCTAssertEqual(Pairing.commitment(ipadFingerprint: ipad.fingerprint, ipadNonce: nonceI), commit,
                       "the iPad must commit to its nonce before seeing ours")
        let code = Pairing.formatSAS(Pairing.sas(daemonFingerprint: daemonID.fingerprint,
                                                 ipadFingerprint: ipad.fingerprint,
                                                 daemonNonce: nonceD, ipadNonce: nonceI))
        try await waitUntil { delegate.pairingCodes == [code] }  // both screens show the same code
        guard case .pairDecision(true) = try await d.next() else { return XCTFail("expected PairDecision(true)") }
        d.send(.pairDecision(true))
        try await waitUntil { delegate.isConnected }
        XCTAssertTrue(trust.contains(daemonID.fingerprint))
        XCTAssertTrue(server.hasActiveSession)

        // clock sync
        d.send(.ping(Ping(id: 7, t1Ns: 123)))
        guard case .pong(let pong) = try await d.next(where: { if case .pong = $0 { return true } else { return false } })
        else { return XCTFail("expected Pong") }
        XCTAssertEqual(pong.id, 7)
        XCTAssertEqual(pong.t1Ns, 123)
        XCTAssertLessThanOrEqual(pong.t2Ns, pong.t3Ns)

        // pen samples flow iPad -> daemon
        let sample = PenSample(timestampNs: 1, seq: 1, phase: .down, flags: 0, x: 0.5, y: 0.5, pressure: 0.3,
                               altitude: 1, azimuth: 0, roll: 0, zOffset: 0, estimationIndex: 0)
        server.send(.penBatch(PenBatch(sendTimeNs: 2, samples: [sample])))
        guard case .penBatch(let batch) = try await d.next(where: { if case .penBatch = $0 { return true } else { return false } })
        else { return XCTFail("expected PenBatch") }
        XCTAssertEqual(batch.samples, [sample])

        // haptic daemon -> iPad
        d.send(.haptic(Haptic(kind: .alignment, x: .nan, y: .nan)))
        try await waitUntil { delegate.haptics.count == 1 }

        // --- second connection from the same, now trusted daemon: no pairing,
        // and it takes over from the first one.
        let d2 = TestDaemon(identity: daemonID, port: port)
        try await waitUntilReady(d2)
        d2.send(hello(trusts: true))
        guard case .helloAck(let ack2) = try await d2.next() else { return XCTFail("expected HelloAck") }
        XCTAssertTrue(ack2.trustsPeer)
        d2.send(.ping(Ping(id: 1, t1Ns: 1)))
        guard case .pong = try await d2.next(where: { if case .pong = $0 { return true } else { return false } })
        else { return XCTFail("expected Pong on 2nd connection") }
        guard case .goodbye(let code1, _) = try await d.next(where: { if case .goodbye = $0 { return true } else { return false } })
        else { return XCTFail("old session should be told it was replaced") }
        XCTAssertEqual(code1, .busy)
        let pairings = await MainActor.run { delegate.pairingCodes.count }
        XCTAssertEqual(pairings, 1, "no second pairing")

        d.cancel()
        d2.cancel()
        server.stop()
    }

    func testRejectedPairingPinsNothingAndUnauthenticatedPeerCannotKickSession() async throws {
        let ipad = try DeviceIdentity.loadOrCreate(ipadNS)
        let trusted = try DeviceIdentity.loadOrCreate(daemonNS)
        let stranger = try DeviceIdentity.loadOrCreate(otherNS)
        let trust = TrustStore(defaultsKey: "test.trust.\(UUID().uuidString)")
        trust.add(trusted.fingerprint, name: "trusted")
        let port = UInt16.random(in: 42000...48000)
        let server = PeerServer(identity: ipad, trust: trust, usbPort: port)
        let delegate = await MainActor.run { RecordingDelegate() }
        await MainActor.run {
            delegate.acceptPairing = false
            server.delegate = delegate
        }
        server.start(wifi: false)
        try await waitUntil { !delegate.statuses.isEmpty }

        // A trusted daemon gets a session...
        let good = TestDaemon(identity: trusted, port: port)
        try await waitUntilReady(good)
        good.send(hello(trusts: true))
        guard case .helloAck = try await good.next() else { return XCTFail("expected HelloAck") }
        try await waitUntil { delegate.isConnected }

        // ...and a stranger that the user rejects neither gets pinned nor
        // disturbs the existing session.
        let bad = TestDaemon(identity: stranger, port: port)
        try await waitUntilReady(bad)
        bad.send(hello(trusts: false, name: "stranger"))
        guard case .helloAck = try await bad.next() else { return XCTFail("expected HelloAck") }
        guard case .pairCommit = try await bad.next() else { return XCTFail("expected PairCommit") }
        bad.send(.pairNonce([UInt8](repeating: 9, count: 32)))
        guard case .pairReveal = try await bad.next() else { return XCTFail("expected PairReveal") }
        guard case .pairDecision(false) = try await bad.next() else { return XCTFail("expected PairDecision(false)") }
        try await waitUntil { delegate.pairingResults == [false] }
        XCTAssertFalse(trust.contains(stranger.fingerprint))

        good.send(.ping(Ping(id: 2, t1Ns: 2)))
        guard case .pong = try await good.next(where: { if case .pong = $0 { return true } else { return false } })
        else { return XCTFail("trusted session must survive") }
        XCTAssertTrue(server.hasActiveSession)

        good.cancel()
        bad.cancel()
        server.stop()
    }

    private func waitUntilReady(_ d: TestDaemon, timeout: TimeInterval = 5) async throws {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if d.isReady { return }
            if d.isClosed { return XCTFail("TLS connection failed") }
            try await Task.sleep(nanoseconds: 10_000_000)
        }
        XCTFail("TLS connection not ready within \(timeout) s")
    }
}

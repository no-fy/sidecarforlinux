import Foundation
import Network
import Security
import SidecarProtocol

/// Events the server reports to the UI (always delivered on the main queue).
@MainActor
protocol PeerServerDelegate: AnyObject {
    func serverStatusChanged(_ status: PeerServer.Status)
    /// Show the pairing code; call `decide` with the user's answer.
    func pairingRequested(code: String, peerName: String, peerFingerprint: String, decide: @escaping (Bool) -> Void)
    func pairingFinished(accepted: Bool, peerName: String)
    func hapticRequested(_ haptic: Haptic)
    func latencyReported(_ report: LatencyReport)
    func mappingReported(_ mapping: MappingInfo)
}

/// Builds TLS 1.3 parameters with our identity, requiring a client
/// certificate. `accept` decides at handshake time whether a peer
/// certificate (DER) may proceed; the pairing/trust decision proper happens
/// after the handshake. Kept outside any actor: the verify block runs on
/// `queue` (see docs/research notes on Swift 6 isolation).
func makeTLSParameters(identity: SecIdentity, queue: DispatchQueue,
                       accept: @escaping ([UInt8]) -> Bool) -> NWParameters {
    let tls = NWProtocolTLS.Options()
    let sec = tls.securityProtocolOptions
    if let sid = sec_identity_create(identity) {
        sec_protocol_options_set_local_identity(sec, sid)
    }
    sec_protocol_options_set_min_tls_protocol_version(sec, .TLSv13)
    sec_protocol_options_set_peer_authentication_required(sec, true)
    sec_protocol_options_add_tls_application_protocol(sec, SidecarProtocol.alpn)
    sec_protocol_options_set_verify_block(sec, { _, trust, complete in
        let secTrust = sec_trust_copy_ref(trust).takeRetainedValue()
        var der: [UInt8] = []
        if let chain = SecTrustCopyCertificateChain(secTrust) as? [SecCertificate], let leaf = chain.first {
            der = [UInt8](SecCertificateCopyData(leaf) as Data)
        }
        complete(!der.isEmpty && accept(der))
    }, queue)

    let tcp = NWProtocolTCP.Options()
    tcp.noDelay = true
    tcp.enableKeepalive = true
    tcp.keepaliveIdle = 2
    tcp.keepaliveInterval = 1
    tcp.keepaliveCount = 3
    let params = NWParameters(tls: tls, tcp: tcp)
    params.allowLocalEndpointReuse = true
    params.serviceClass = .interactiveVideo
    return params
}

/// Peer certificate of an established TLS connection.
func peerCertificateDER(of connection: NWConnection) -> [UInt8]? {
    guard let md = connection.metadata(definition: NWProtocolTLS.definition) as? NWProtocolTLS.Metadata else {
        return nil
    }
    var der: [UInt8]?
    sec_protocol_metadata_access_peer_certificate_chain(md.securityProtocolMetadata) { cert in
        if der == nil {
            let ref = sec_certificate_copy_ref(cert).takeRetainedValue()
            der = [UInt8](SecCertificateCopyData(ref) as Data)
        }
    }
    return der
}

/// Listens for the Linux daemon:
/// * USB: 127.0.0.1:47811. usbmuxd on the PC tunnels connections to this
///   loopback port (no Local Network permission needed).
/// * Wi-Fi: port 47812 advertised over Bonjour as `_sidecarlinux._tcp`. Only
///   already-paired PCs get through the TLS handshake, unless the user opened
///   the pairing window in the app.
final class PeerServer {
    enum Status: Equatable {
        case starting
        case waiting(wifi: Bool)
        case handshaking(viaUSB: Bool)
        case pairing(peer: String)
        case connected(peer: String, viaUSB: Bool)
        case failed(String)
    }

    static let wifiPort: UInt16 = 47812

    let queue = DispatchQueue(label: "sidecar.net", qos: .userInteractive)
    // Only touched on the main queue.
    weak var delegate: PeerServerDelegate?
    private(set) var identity: DeviceIdentity
    let trust: TrustStore
    private var usbListener: NWListener?
    private var wifiListener: NWListener?
    private var current: PeerConnection?
    private var wifiPairingUntil: Date?
    private var wifiEnabled = true
    var surface = SurfaceInfo(widthPt: 1180, heightPt: 820, scale: 2, pointsPerInch: 132)
    var capabilities: UInt32 = Caps.gestures | Caps.haptics | Caps.hover | Caps.roll | Caps.predicted

    init(identity: DeviceIdentity, trust: TrustStore) {
        self.identity = identity
        self.trust = trust
    }

    // MARK: lifecycle (call from main)

    /// Idempotent: (re)creates only listeners that are missing or dead, so
    /// calling it at launch and on every foregrounding is safe.
    func start(wifi: Bool) {
        queue.async {
            self.wifiEnabled = wifi
            if !Self.alive(self.usbListener) {
                self.usbListener?.cancel()
                self.startUSBListener()
            }
            if wifi && !Self.alive(self.wifiListener) {
                self.wifiListener?.cancel()
                self.startWifiListener()
            }
            self.reportWaitingIfIdle()
        }
    }

    private static func alive(_ l: NWListener?) -> Bool {
        guard let l else { return false }
        switch l.state {
        case .failed, .cancelled: return false
        default: return true
        }
    }

    func stop() {
        queue.async {
            self.usbListener?.cancel()
            self.wifiListener?.cancel()
            self.usbListener = nil
            self.wifiListener = nil
            self.current?.close(code: .shuttingDown, reason: "iPad app went to the background")
            self.current = nil
        }
    }

    func setWifi(enabled: Bool) {
        queue.async {
            self.wifiEnabled = enabled
            self.wifiListener?.cancel()
            self.wifiListener = nil
            if enabled { self.startWifiListener() }
            self.reportWaitingIfIdle()
        }
    }

    /// Lets unknown PCs reach the pairing step over Wi-Fi for two minutes.
    func openWifiPairingWindow() {
        queue.async { self.wifiPairingUntil = Date().addingTimeInterval(120) }
    }

    func replaceIdentity(_ id: DeviceIdentity) {
        queue.async {
            self.identity = id
            self.current?.close(code: .normal, reason: "identity reset")
            self.current = nil
            self.usbListener?.cancel()
            self.wifiListener?.cancel()
            self.startListeners()
        }
    }

    func updateSurface(_ s: SurfaceInfo) {
        queue.async {
            guard s != self.surface else { return }
            self.surface = s
            self.current?.send(.surfaceInfo(s))
        }
    }

    // MARK: sending (any thread)

    func send(_ m: Message) {
        queue.async { self.current?.sendIfActive(m) }
    }

    var hasActiveSession: Bool {
        queue.sync { current?.isActive ?? false }
    }

    // MARK: internals (on queue)

    private func startListeners() {
        startUSBListener()
        if wifiEnabled { startWifiListener() }
        reportWaitingIfIdle()
    }

    private func startUSBListener() {
        let params = makeTLSParameters(identity: identity.identity, queue: queue) { _ in true }
        params.requiredLocalEndpoint = NWEndpoint.hostPort(host: .ipv4(.loopback),
                                                           port: NWEndpoint.Port(rawValue: SidecarProtocol.defaultPort)!)
        do {
            let l = try NWListener(using: params)
            l.newConnectionHandler = { [weak self] c in self?.accept(c, viaUSB: true) }
            l.stateUpdateHandler = { [weak self] st in self?.listenerState(st, name: "USB") }
            l.start(queue: queue)
            usbListener = l
        } catch {
            report(.failed("USB listener: \(error)"))
        }
    }

    private func startWifiListener() {
        let params = makeTLSParameters(identity: identity.identity, queue: queue) { [weak self] der in
            // Runs on `queue`.
            guard let self else { return false }
            if self.trust.contains(Pairing.fingerprint(certificateDER: der)) { return true }
            if let until = self.wifiPairingUntil, until > Date() { return true }
            return false
        }
        params.prohibitedInterfaceTypes = [.cellular, .loopback]
        do {
            let l = try NWListener(using: params, on: NWEndpoint.Port(rawValue: Self.wifiPort)!)
            let txt = NWTXTRecord(["fp": Pairing.hex(identity.fingerprint), "v": "\(SidecarProtocol.major)"])
            l.service = NWListener.Service(name: nil, type: SidecarProtocol.bonjourServiceType, domain: nil,
                                           txtRecord: txt)
            l.newConnectionHandler = { [weak self] c in self?.accept(c, viaUSB: false) }
            l.stateUpdateHandler = { [weak self] st in self?.listenerState(st, name: "Wi-Fi") }
            l.start(queue: queue)
            wifiListener = l
        } catch {
            report(.failed("Wi-Fi listener: \(error)"))
        }
    }

    private func listenerState(_ st: NWListener.State, name: String) {
        switch st {
        case .failed(let e):
            report(.failed("\(name) listener failed: \(e) (retrying)"))
            // e.g. the port is still held right after returning from the
            // background: try again shortly.
            queue.asyncAfter(deadline: .now() + 2) { [weak self] in
                guard let self else { return }
                self.start(wifi: self.wifiEnabled)
            }
        case .ready:
            reportWaitingIfIdle()
        default:
            break
        }
    }

    private func accept(_ c: NWConnection, viaUSB: Bool) {
        // The newest authenticated connection wins (a daemon reconnecting
        // after a cable wiggle shouldn't wait for the old socket to time out).
        let pc = PeerConnection(connection: c, viaUSB: viaUSB, server: self)
        pc.start()
    }

    fileprivate func connectionReady(_ pc: PeerConnection) {
        if let old = current, old !== pc {
            old.close(code: .busy, reason: "replaced by a newer connection")
        }
        current = pc
        report(.handshaking(viaUSB: pc.viaUSB))
    }

    fileprivate func connectionClosed(_ pc: PeerConnection) {
        if current === pc {
            current = nil
            reportWaitingIfIdle()
        }
    }

    private func reportWaitingIfIdle() {
        if current == nil { report(.waiting(wifi: wifiEnabled)) }
    }

    fileprivate func report(_ s: Status) {
        onMain { $0.serverStatusChanged(s) }
    }

    fileprivate func onMain(_ f: @escaping @MainActor (PeerServerDelegate) -> Void) {
        DispatchQueue.main.async {
            MainActor.assumeIsolated {
                if let d = self.delegate { f(d) }
            }
        }
    }
}

/// One TLS connection from the daemon: hello, optional pairing, session.
final class PeerConnection {
    enum State {
        case awaitingHello
        case pairingAwaitNonce(nonce: [UInt8])
        case pairingAwaitDecisions(ours: Bool?, theirs: Bool?)
        case active
        case closed
    }

    let connection: NWConnection
    let viaUSB: Bool
    private unowned let server: PeerServer
    private var decoder = FrameDecoder()
    private var state: State = .awaitingHello
    private var peerFingerprint: [UInt8] = []
    private var peerName = "PC"
    private var receivedAt: UInt64 = 0

    init(connection: NWConnection, viaUSB: Bool, server: PeerServer) {
        self.connection = connection
        self.viaUSB = viaUSB
        self.server = server
    }

    var isActive: Bool {
        if case .active = state { return true }
        return false
    }

    func start() {
        connection.stateUpdateHandler = { [weak self] st in
            guard let self else { return }
            switch st {
            case .ready:
                guard let der = peerCertificateDER(of: self.connection) else {
                    self.close(code: .protocolError, reason: "no client certificate")
                    return
                }
                self.peerFingerprint = Pairing.fingerprint(certificateDER: der)
                self.server.connectionReady(self)
                self.receive()
            case .failed, .cancelled:
                self.state = .closed
                self.server.connectionClosed(self)
            default:
                break
            }
        }
        connection.start(queue: server.queue)
    }

    func close(code: GoodbyeCode, reason: String) {
        if case .closed = state { return }
        state = .closed
        let c = connection
        // Cancel only once the Goodbye has been handed to the network.
        c.send(content: Data(Message.goodbye(code: code, reason: reason).encode()),
               completion: .contentProcessed { _ in c.cancel() })
        server.connectionClosed(self)
    }

    func send(_ m: Message) {
        connection.send(content: Data(m.encode()), completion: .contentProcessed { _ in })
    }

    func sendIfActive(_ m: Message) {
        if isActive { send(m) }
    }

    private func receive() {
        connection.receive(minimumIncompleteLength: 1, maximumLength: 256 * 1024) { [weak self] data, _, done, error in
            guard let self else { return }
            if let data, !data.isEmpty {
                self.receivedAt = SessionClock.nowNs()
                self.decoder.push([UInt8](data))
                do {
                    while let f = try self.decoder.nextFrame() {
                        try self.handle(try Message.decode(type: f.type, payload: f.payload))
                    }
                } catch {
                    self.close(code: .protocolError, reason: "\(error)")
                    return
                }
            }
            if done || error != nil {
                self.state = .closed
                self.connection.cancel()
                self.server.connectionClosed(self)
                return
            }
            if case .closed = self.state { return }
            self.receive()
        }
    }

    private func handle(_ m: Message) throws {
        switch (state, m) {
        case (_, .goodbye(let code, let reason)):
            if code == .notPaired {
                server.report(.failed("The PC refused: this iPad is not paired. Run `sidecard pair` on the PC."))
            } else if code != .normal {
                server.report(.failed("PC closed the session: \(reason)"))
            }
            state = .closed
            connection.cancel()
            server.connectionClosed(self)

        case (.awaitingHello, .hello(let h)):
            guard h.major == SidecarProtocol.major, h.channel == .input else {
                close(code: .versionMismatch, reason: "iPad speaks protocol \(SidecarProtocol.major).\(SidecarProtocol.minor)")
                return
            }
            peerName = h.name
            let weTrust = server.trust.contains(peerFingerprint)
            send(.helloAck(HelloAck(trustsPeer: weTrust, caps: server.capabilities, surface: server.surface,
                                    deviceName: deviceName(), model: "iPad")))
            if weTrust && h.trustsPeer {
                activate()
            } else {
                var nonce = [UInt8](repeating: 0, count: 32)
                _ = SecRandomCopyBytes(kSecRandomDefault, 32, &nonce)
                state = .pairingAwaitNonce(nonce: nonce)
                server.report(.pairing(peer: peerName))
                send(.pairCommit(Pairing.commitment(ipadFingerprint: server.identity.fingerprint, ipadNonce: nonce)))
            }

        case (.pairingAwaitNonce(let nonce), .pairNonce(let daemonNonce)):
            send(.pairReveal(nonce))
            state = .pairingAwaitDecisions(ours: nil, theirs: nil)
            let code = Pairing.sas(daemonFingerprint: peerFingerprint, ipadFingerprint: server.identity.fingerprint,
                                   daemonNonce: daemonNonce, ipadNonce: nonce)
            let name = peerName
            let fp = Pairing.formatFingerprint(peerFingerprint)
            server.onMain { d in
                d.pairingRequested(code: Pairing.formatSAS(code), peerName: name, peerFingerprint: fp) { ok in
                    self.server.queue.async { self.userDecided(ok) }
                }
            }

        case (.pairingAwaitDecisions(let ours, _), .pairDecision(let theirs)):
            state = .pairingAwaitDecisions(ours: ours, theirs: theirs)
            finishPairingIfDecided()

        case (.active, .ping(let p)):
            send(.pong(Pong(id: p.id, t1Ns: p.t1Ns, t2Ns: receivedAt, t3Ns: SessionClock.nowNs())))
        case (.active, .haptic(let h)):
            server.onMain { $0.hapticRequested(h) }
        case (.active, .latencyReport(let r)):
            server.onMain { $0.latencyReported(r) }
        case (.active, .mappingInfo(let mi)):
            server.onMain { $0.mappingReported(mi) }
        case (.active, _):
            break  // unknown/unused messages are ignored (forward compatibility)

        case (_, .ping(let p)):
            // Clock sync may start before the session is active.
            send(.pong(Pong(id: p.id, t1Ns: p.t1Ns, t2Ns: receivedAt, t3Ns: SessionClock.nowNs())))
        default:
            close(code: .protocolError, reason: "unexpected message in this state")
        }
    }

    private func userDecided(_ ok: Bool) {
        guard case .pairingAwaitDecisions(_, let theirs) = state else { return }
        send(.pairDecision(ok))
        state = .pairingAwaitDecisions(ours: ok, theirs: theirs)
        finishPairingIfDecided()
    }

    private func finishPairingIfDecided() {
        guard case .pairingAwaitDecisions(let ours, let theirs) = state else { return }
        if ours == false || theirs == false {
            let name = peerName
            server.onMain { $0.pairingFinished(accepted: false, peerName: name) }
            close(code: .pairingRejected, reason: "pairing rejected")
            return
        }
        guard ours == true, theirs == true else { return }
        server.trust.add(peerFingerprint, name: peerName)
        let name = peerName
        server.onMain { $0.pairingFinished(accepted: true, peerName: name) }
        activate()
    }

    private func activate() {
        state = .active
        server.report(.connected(peer: peerName, viaUSB: viaUSB))
    }

    private func deviceName() -> String {
        // UIDevice.name is "iPad" without a special entitlement; good enough.
        "iPad"
    }
}

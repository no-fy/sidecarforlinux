#if canImport(UIKit)
import SidecarProtocol
import SidecarNet
import UIKit

/// Glue between the pencil surface, the network server and the UI.
@MainActor
final class AppController: PeerServerDelegate, PencilCaptureDelegate {
    private unowned let root: RootViewController
    private var server: PeerServer?
    private let trust = TrustStore()
    private var identity: DeviceIdentity?
    private var connected = false
    private var pendingPairing: UIAlertController?
    /// Index 0: AVSampleBufferDisplayLayer, 1: Metal (see `Settings.metalVideo`).
    private var videoSwitch: VideoSinkSwitch?

    init(root: RootViewController) {
        self.root = root
    }

    func start() {
        root.capture.delegate = self
        do {
            let id = try DeviceIdentity.loadOrCreate()
            identity = id
            let s = PeerServer(identity: id, trust: trust)
            s.delegate = self
            let sw = VideoSinkSwitch([root.video.sink, root.metalVideo.sink], selected: Settings.metalVideo ? 1 : 0)
            videoSwitch = sw
            s.videoSink = sw
            server = s
            root.setFingerprint(Pairing.formatFingerprint(id.fingerprint))
            s.updateSurface(root.surfaceInfo())
            s.start(wifi: Settings.wifiEnabled)
        } catch {
            root.setStatus("Could not create the TLS identity: \(error)", ok: false)
        }
    }

    func sceneBecameActive() {
        if let server, identity != nil {
            server.start(wifi: Settings.wifiEnabled)
        }
    }

    func sceneWillResignActive() {
        root.capture.forceOut()
    }

    func surfaceChanged(_ s: SurfaceInfo) {
        server?.updateSurface(s)
    }

    // MARK: settings actions

    func setWifi(_ on: Bool) {
        Settings.wifiEnabled = on
        server?.setWifi(enabled: on)
    }

    func openWifiPairing() {
        server?.openWifiPairingWindow()
        root.setStatus("Wi-Fi pairing open for 2 minutes: run `sidecard pair --transport wifi` on the PC", ok: true)
    }

    func setMetalVideo(_ on: Bool) {
        Settings.metalVideo = on
        videoSwitch?.select(on ? 1 : 0)
        root.updateVideoViews()
    }

    func forgetPCs() {
        trust.removeAll()
        root.setStatus("Forgot all paired PCs", ok: true)
    }

    func resetIdentity() {
        do {
            let id = try DeviceIdentity.reset()
            identity = id
            trust.removeAll()
            server?.replaceIdentity(id)
            root.setFingerprint(Pairing.formatFingerprint(id.fingerprint))
        } catch {
            root.setStatus("Reset failed: \(error)", ok: false)
        }
    }

    // MARK: PencilCaptureDelegate

    func capture(_ view: PencilCaptureView, batch: PenBatch) {
        guard connected else { return }
        server?.send(.penBatch(batch))
    }

    func capture(_ view: PencilCaptureView, gesture: PencilGesture) {
        root.flashGesture(gesture)
        guard connected else { return }
        server?.send(.pencilGesture(gesture))
    }

    // MARK: PeerServerDelegate (main queue)

    func serverStatusChanged(_ status: PeerServer.Status) {
        switch status {
        case .starting:
            connected = false
            root.setStatus("Starting…", ok: true)
        case .waiting(let wifi):
            connected = false
            videoEnded()
            UIApplication.shared.isIdleTimerDisabled = false
            root.setStatus(wifi ? "Waiting for the PC (USB or Wi-Fi)…" : "Waiting for the PC over USB…", ok: true)
        case .handshaking(let usb):
            root.setStatus("PC connecting over \(usb ? "USB" : "Wi-Fi")…", ok: true)
        case .pairing(let peer):
            root.setStatus("Pairing with \(peer)…", ok: true)
        case .connected(let peer, let usb):
            connected = true
            UIApplication.shared.isIdleTimerDisabled = true
            root.setStatus("Connected to \(peer) over \(usb ? "USB" : "Wi-Fi")", ok: true)
        case .failed(let why):
            connected = false
            root.setStatus(why, ok: false)
        }
    }

    func pairingRequested(code: String, peerName: String, peerFingerprint: String, decide: @escaping (Bool) -> Void) {
        let alert = UIAlertController(
            title: "Pair with \(peerName)?",
            message: "Check that the PC shows the same code:\n\n\(code)\n\nPC certificate:\n\(peerFingerprint.prefix(47))…",
            preferredStyle: .alert)
        alert.addAction(UIAlertAction(title: "Codes differ", style: .destructive) { _ in decide(false) })
        alert.addAction(UIAlertAction(title: "Codes match, pair", style: .default) { _ in decide(true) })
        pendingPairing = alert
        root.present(alert, animated: true)
    }

    func pairingFinished(accepted: Bool, peerName: String) {
        pendingPairing?.dismiss(animated: true)
        pendingPairing = nil
        root.setStatus(accepted ? "Paired with \(peerName)" : "Pairing with \(peerName) cancelled", ok: accepted)
    }

    func hapticRequested(_ haptic: Haptic) {
        root.capture.playHaptic(haptic)
    }

    func latencyReported(_ r: LatencyReport) {
        guard r.samples > 0 else { return }
        root.setLatency(String(format: "touch→PC p50 %.1f ms · p95 %.1f ms · rtt %.2f ms",
                               Double(r.p50Us) / 1000, Double(r.p95Us) / 1000, Double(r.rttUs) / 1000))
    }

    func mappingReported(_ mapping: MappingInfo) {}

    // Frames themselves go to `root.video.sink` on the network queue.
    func videoConfigReceived(_ config: VideoConfig) {
        root.setVideoVisible(true)
    }

    func videoEnded() {
        root.setVideoVisible(false)
    }
}

enum Settings {
    static var wifiEnabled: Bool {
        get { UserDefaults.standard.object(forKey: "wifiEnabled") as? Bool ?? true }
        set { UserDefaults.standard.set(newValue, forKey: "wifiEnabled") }
    }
    static var sendPredicted: Bool {
        get { UserDefaults.standard.bool(forKey: "sendPredicted") }
        set { UserDefaults.standard.set(newValue, forKey: "sendPredicted") }
    }
    static var showInk: Bool {
        get { UserDefaults.standard.object(forKey: "showInk") as? Bool ?? true }
        set { UserDefaults.standard.set(newValue, forKey: "showInk") }
    }
    /// Video renderer: Metal (true) or AVSampleBufferDisplayLayer (false,
    /// the default until the Metal path is confirmed on a device).
    static var metalVideo: Bool {
        get { UserDefaults.standard.bool(forKey: "metalVideo") }
        set { UserDefaults.standard.set(newValue, forKey: "metalVideo") }
    }
}

final class RootViewController: UIViewController {
    weak var controller: AppController?
    let capture = PencilCaptureView(frame: .zero)
    let video = VideoView(frame: .zero)
    let metalVideo = MetalVideoView(frame: .zero)
    private var videoOn = false
    private let statusLabel = UILabel()
    private let latencyLabel = UILabel()
    private let gestureLabel = UILabel()
    private let menuButton = UIButton(type: .system)
    private var fingerprint = ""
    private var lastSurface: SurfaceInfo?

    override var prefersStatusBarHidden: Bool { true }
    override var prefersHomeIndicatorAutoHidden: Bool { true }
    override var preferredScreenEdgesDeferringSystemGestures: UIRectEdge { .all }

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .black
        for v in [video, metalVideo] {
            v.translatesAutoresizingMaskIntoConstraints = false
            v.isHidden = true
            view.addSubview(v)
        }
        metalVideo.onError = { [weak self] message in self?.setStatus("Metal video: \(message)", ok: false) }
        capture.translatesAutoresizingMaskIntoConstraints = false
        capture.sendPredicted = Settings.sendPredicted
        capture.showInk = Settings.showInk
        view.addSubview(capture)

        for l in [statusLabel, latencyLabel, gestureLabel] {
            l.translatesAutoresizingMaskIntoConstraints = false
            l.textColor = UIColor(white: 0.85, alpha: 1)
            l.font = .monospacedSystemFont(ofSize: 13, weight: .regular)
            l.numberOfLines = 0
            l.isUserInteractionEnabled = false
            view.addSubview(l)
        }
        gestureLabel.textColor = .systemTeal
        gestureLabel.alpha = 0

        var cfg = UIButton.Configuration.gray()
        cfg.image = UIImage(systemName: "gearshape")
        menuButton.configuration = cfg
        menuButton.translatesAutoresizingMaskIntoConstraints = false
        menuButton.showsMenuAsPrimaryAction = true
        view.addSubview(menuButton)
        rebuildMenu()

        let g = view.safeAreaLayoutGuide
        NSLayoutConstraint.activate([
            video.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            video.trailingAnchor.constraint(equalTo: view.trailingAnchor),
            video.topAnchor.constraint(equalTo: view.topAnchor),
            video.bottomAnchor.constraint(equalTo: view.bottomAnchor),
            metalVideo.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            metalVideo.trailingAnchor.constraint(equalTo: view.trailingAnchor),
            metalVideo.topAnchor.constraint(equalTo: view.topAnchor),
            metalVideo.bottomAnchor.constraint(equalTo: view.bottomAnchor),
            capture.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            capture.trailingAnchor.constraint(equalTo: view.trailingAnchor),
            capture.topAnchor.constraint(equalTo: view.topAnchor),
            capture.bottomAnchor.constraint(equalTo: view.bottomAnchor),
            menuButton.topAnchor.constraint(equalTo: g.topAnchor, constant: 12),
            menuButton.trailingAnchor.constraint(equalTo: g.trailingAnchor, constant: -12),
            statusLabel.topAnchor.constraint(equalTo: g.topAnchor, constant: 16),
            statusLabel.leadingAnchor.constraint(equalTo: g.leadingAnchor, constant: 16),
            statusLabel.trailingAnchor.constraint(lessThanOrEqualTo: menuButton.leadingAnchor, constant: -12),
            latencyLabel.topAnchor.constraint(equalTo: statusLabel.bottomAnchor, constant: 6),
            latencyLabel.leadingAnchor.constraint(equalTo: statusLabel.leadingAnchor),
            gestureLabel.bottomAnchor.constraint(equalTo: g.bottomAnchor, constant: -16),
            gestureLabel.centerXAnchor.constraint(equalTo: g.centerXAnchor),
        ])
    }

    override func viewDidLayoutSubviews() {
        super.viewDidLayoutSubviews()
        let s = surfaceInfo()
        if s != lastSurface {
            lastSurface = s
            controller?.surfaceChanged(s)
        }
    }

    func surfaceInfo() -> SurfaceInfo {
        let b = capture.bounds.size == .zero ? view.bounds.size : capture.bounds.size
        // 264 ppi iPads (all current iPad Air/Pro) are 132 points per inch;
        // the iPad mini (326 ppi) is 163.
        let ppi: Float = UIDevice.current.userInterfaceIdiom == .pad && max(b.width, b.height) < 1200 &&
            min(b.width, b.height) <= 744 ? 163 : 132
        return SurfaceInfo(widthPt: Float(b.width), heightPt: Float(b.height),
                           scale: Float(traitCollection.displayScale), pointsPerInch: ppi)
    }

    func setStatus(_ text: String, ok: Bool) {
        statusLabel.text = text
        statusLabel.textColor = ok ? UIColor(white: 0.85, alpha: 1) : .systemOrange
    }

    /// With video, the pencil surface becomes transparent over the PC's screen.
    func setVideoVisible(_ on: Bool) {
        videoOn = on
        updateVideoViews()
        capture.backgroundColor = on ? .clear : UIColor(white: 0.08, alpha: 1)
        statusLabel.alpha = on ? 0.6 : 1
    }

    /// Shows the view of the selected renderer (⚙︎ → Video renderer).
    func updateVideoViews() {
        video.isHidden = !(videoOn && !Settings.metalVideo)
        metalVideo.isHidden = !(videoOn && Settings.metalVideo)
        rebuildMenu()
    }

    func setLatency(_ text: String) {
        latencyLabel.text = text
    }

    func setFingerprint(_ fp: String) {
        fingerprint = fp
        rebuildMenu()
    }

    func flashGesture(_ g: PencilGesture) {
        let name: String
        switch g.kind {
        case .doubleTap: name = "double-tap"
        case .squeeze: name = "squeeze \(g.phase)"
        }
        gestureLabel.text = name
        gestureLabel.alpha = 1
        UIView.animate(withDuration: 0.8, delay: 0.4) { self.gestureLabel.alpha = 0 }
    }

    private func rebuildMenu() {
        let wifi = UIAction(title: "Wi-Fi fallback", image: UIImage(systemName: "wifi"),
                            state: Settings.wifiEnabled ? .on : .off) { [weak self] _ in
            Settings.wifiEnabled.toggle()
            self?.controller?.setWifi(Settings.wifiEnabled)
            self?.rebuildMenu()
        }
        let pairWifi = UIAction(title: "Allow pairing over Wi-Fi (2 min)", image: UIImage(systemName: "link")) {
            [weak self] _ in self?.controller?.openWifiPairing()
        }
        let predicted = UIAction(title: "Send predicted touches", image: UIImage(systemName: "wand.and.rays"),
                                 state: Settings.sendPredicted ? .on : .off) { [weak self] _ in
            Settings.sendPredicted.toggle()
            self?.capture.sendPredicted = Settings.sendPredicted
            self?.rebuildMenu()
        }
        let ink = UIAction(title: "Show local ink", image: UIImage(systemName: "scribble"),
                           state: Settings.showInk ? .on : .off) { [weak self] _ in
            Settings.showInk.toggle()
            self?.capture.showInk = Settings.showInk
            self?.rebuildMenu()
        }
        let renderers = [(false, "AVSampleBufferDisplayLayer"), (true, "Metal")].map { metal, name in
            UIAction(title: name, state: Settings.metalVideo == metal ? .on : .off) { [weak self] _ in
                self?.controller?.setMetalVideo(metal)
            }
        }
        let renderer = UIMenu(title: "Video renderer", subtitle: Settings.metalVideo ? "Metal" : "AVSampleBufferDisplayLayer",
                              image: UIImage(systemName: "display"), children: renderers)
        let forget = UIAction(title: "Forget paired PCs", image: UIImage(systemName: "trash"),
                              attributes: .destructive) { [weak self] _ in self?.controller?.forgetPCs() }
        let reset = UIAction(title: "New iPad identity (re-pair everything)",
                             image: UIImage(systemName: "key"), attributes: .destructive) { [weak self] _ in
            self?.controller?.resetIdentity()
        }
        let fp = UIAction(title: "iPad certificate", subtitle: fingerprint.isEmpty ? "…" : String(fingerprint.prefix(23)) + "…",
                          image: UIImage(systemName: "checkmark.shield"), attributes: .disabled) { _ in }
        menuButton.menu = UIMenu(children: [
            UIMenu(options: .displayInline, children: [wifi, pairWifi]),
            UIMenu(options: .displayInline, children: [predicted, ink, renderer]),
            UIMenu(options: .displayInline, children: [fp, forget, reset]),
        ])
    }
}
#endif

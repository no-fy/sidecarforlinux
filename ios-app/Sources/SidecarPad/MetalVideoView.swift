#if canImport(UIKit)
import Metal
import QuartzCore
import SidecarNet
import UIKit

/// Video renderer "Metal": hosts the CAMetalLayer that `MetalVideoSink`
/// (SidecarNet) draws into, keeps its drawable at the view's pixel size,
/// drives the UI-update-link presentation mode (iOS 18+) and stops GPU work
/// while the app is in the background.
final class MetalVideoView: UIView {
    override class var layerClass: AnyClass { CAMetalLayer.self }
    private var metalLayer: CAMetalLayer { layer as! CAMetalLayer }
    /// Handed to `PeerServer.videoSink` (through the renderer switch).
    private(set) var sink: MetalVideoSink!
    /// Problems drawing, on the main queue.
    var onError: ((String) -> Void)?
    /// The sink's once-a-second readout, on the main queue.
    var onStats: ((String) -> Void)?
    /// UIUpdateLink (iOS 18+); stored untyped because stored properties
    /// can't carry availability.
    private var updateLink: AnyObject?

    override init(frame: CGRect) {
        super.init(frame: frame)
        backgroundColor = .black
        isUserInteractionEnabled = false
        sink = MetalVideoSink(
            layer: metalLayer,
            onError: { [weak self] message in
                DispatchQueue.main.async { MainActor.assumeIsolated { self?.onError?(message) } }
            },
            onStats: { [weak self] text in
                DispatchQueue.main.async { MainActor.assumeIsolated { self?.onStats?(text) } }
            })
        let nc = NotificationCenter.default
        nc.addObserver(self, selector: #selector(didEnterBackground),
                       name: UIApplication.didEnterBackgroundNotification, object: nil)
        nc.addObserver(self, selector: #selector(willEnterForeground),
                       name: UIApplication.willEnterForegroundNotification, object: nil)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    override var isHidden: Bool {
        didSet {
            sink?.setActive(!isHidden)
            refreshUpdateLink()
        }
    }

    /// The UI-update-link mode needs iOS 18; older systems get the display link.
    func setPresentMode(_ requested: MetalPresentMode) {
        var mode = requested
        if #available(iOS 18.0, *) {} else if mode == .uiUpdate { mode = .displayLink }
        sink.setMode(mode)
        refreshUpdateLink()
    }

    override func layoutSubviews() {
        super.layoutSubviews()
        let scale = traitCollection.displayScale
        metalLayer.contentsScale = scale
        // One drawable pixel per screen pixel (2360×1640 on an 11" iPad Air),
        // the same size the PC's virtual monitor is created at: no scaling
        // pass in the compositor.
        metalLayer.drawableSize = CGSize(width: bounds.width * scale, height: bounds.height * scale)
    }

    private func refreshUpdateLink() {
        guard #available(iOS 18.0, *), sink != nil else { return }
        let wanted = sink.mode == .uiUpdate && !isHidden
        if wanted && updateLink == nil {
            let link = UIUpdateLink(view: self)
            // A UI update every refresh while the video is up, not only on input.
            link.requiresContinuousUpdates = true
            // "presents one frame duration sooner" (UIUpdateLink docs).
            link.wantsImmediatePresentation = true
            link.addAction(to: .beforeCATransactionCommit) { [weak self] _, info in
                self?.sink.presentForUIUpdate(target: info.estimatedPresentationTime,
                                              immediateExpected: info.isImmediatePresentationExpected)
            }
            updateLink = link
        }
        (updateLink as? UIUpdateLink)?.isEnabled = wanted
    }

    @objc private func didEnterBackground() {
        sink.isPaused = true
    }

    @objc private func willEnterForeground() {
        sink.isPaused = false
    }
}
#endif

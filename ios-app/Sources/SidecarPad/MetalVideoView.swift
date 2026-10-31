#if canImport(UIKit)
import Metal
import QuartzCore
import SidecarNet
import UIKit

/// Video renderer "Metal": hosts the CAMetalLayer that `MetalVideoSink`
/// (SidecarNet) draws into, keeps its drawable at the view's pixel size and
/// stops GPU work while the app is in the background.
final class MetalVideoView: UIView {
    override class var layerClass: AnyClass { CAMetalLayer.self }
    private var metalLayer: CAMetalLayer { layer as! CAMetalLayer }
    /// Handed to `PeerServer.videoSink` (through the renderer switch).
    private(set) var sink: MetalVideoSink!
    /// Problems drawing, on the main queue.
    var onError: ((String) -> Void)?

    override init(frame: CGRect) {
        super.init(frame: frame)
        backgroundColor = .black
        isUserInteractionEnabled = false
        sink = MetalVideoSink(layer: metalLayer) { [weak self] message in
            DispatchQueue.main.async { MainActor.assumeIsolated { self?.onError?(message) } }
        }
        let nc = NotificationCenter.default
        nc.addObserver(self, selector: #selector(didEnterBackground),
                       name: UIApplication.didEnterBackgroundNotification, object: nil)
        nc.addObserver(self, selector: #selector(willEnterForeground),
                       name: UIApplication.willEnterForegroundNotification, object: nil)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    override func layoutSubviews() {
        super.layoutSubviews()
        let scale = traitCollection.displayScale
        metalLayer.contentsScale = scale
        // One drawable pixel per screen pixel (2360×1640 on an 11" iPad Air),
        // the same size the PC's virtual monitor is created at.
        metalLayer.drawableSize = CGSize(width: bounds.width * scale, height: bounds.height * scale)
    }

    @objc private func didEnterBackground() {
        sink.isPaused = true
    }

    @objc private func willEnterForeground() {
        sink.isPaused = false
    }
}
#endif

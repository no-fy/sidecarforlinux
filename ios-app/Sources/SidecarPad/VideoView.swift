#if canImport(UIKit)
import AVFoundation
import QuartzCore
import SidecarNet
import SidecarProtocol
import UIKit

/// Video renderer "AVSampleBufferDisplayLayer": shows the PC's virtual
/// display (phase 2). Frames go straight into an AVSampleBufferDisplayLayer
/// marked "display immediately": no pacing, no extra buffering. They are
/// enqueued by `sink` (SidecarNet's LayerVideoSink), on the network queue.
/// While the view is visible, a display link asks the layer once per
/// refresh what it shows, so the sink can time each frame.
final class VideoView: UIView {
    override class var layerClass: AnyClass { AVSampleBufferDisplayLayer.self }
    private var displayLayer: AVSampleBufferDisplayLayer { layer as! AVSampleBufferDisplayLayer }
    /// Handed to `PeerServer.videoSink` (through the renderer switch).
    private(set) var sink: LayerVideoSink!
    /// The sink's once-a-second readout, on the main queue.
    var onStats: ((String) -> Void)?
    private var poller: RefreshPoller?

    override init(frame: CGRect) {
        super.init(frame: frame)
        backgroundColor = .black
        displayLayer.videoGravity = .resizeAspect
        isUserInteractionEnabled = false
        let s = LayerVideoSink(renderer: displayLayer.sampleBufferRenderer) { [weak self] text in
            DispatchQueue.main.async { MainActor.assumeIsolated { self?.onStats?(text) } }
        }
        sink = s
        poller = RefreshPoller { refresh in s.pollDisplayed(atNs: SessionClock.ns(fromMediaTime: refresh)) }
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    override var isHidden: Bool {
        didSet {
            // Measure (and poll) only while this renderer is on screen.
            sink?.setMeasuring(!isHidden)
            poller?.setRunning(!isHidden)
        }
    }
}

/// Calls `onRefresh` with each display refresh's time (CADisplayLink
/// `timestamp`, media time) on its own thread while running, so asking the
/// layer for its picture never competes with pencil touches on the main
/// thread.
final class RefreshPoller: NSObject, @unchecked Sendable {
    private let onRefresh: @Sendable (CFTimeInterval) -> Void
    private var runLoop: CFRunLoop?
    private var link: CADisplayLink?  // run loop thread only

    init(onRefresh: @escaping @Sendable (CFTimeInterval) -> Void) {
        self.onRefresh = onRefresh
        super.init()
        let ready = DispatchSemaphore(value: 0)
        let t = Thread { [self] in
            runLoop = CFRunLoopGetCurrent()
            RunLoop.current.add(NSMachPort(), forMode: .default)  // keeps the run loop alive while paused
            let l = CADisplayLink(target: self, selector: #selector(refresh(_:)))
            l.isPaused = true
            l.add(to: .current, forMode: .default)
            link = l
            ready.signal()
            while true { _ = RunLoop.current.run(mode: .default, before: .distantFuture) }
        }
        t.name = "sidecar.video.layerpoll"
        t.qualityOfService = .userInteractive
        t.start()
        ready.wait()
    }

    func setRunning(_ on: Bool) {
        guard let runLoop else { return }
        CFRunLoopPerformBlock(runLoop, CFRunLoopMode.defaultMode.rawValue) { self.link?.isPaused = !on }
        CFRunLoopWakeUp(runLoop)
    }

    @objc private func refresh(_ l: CADisplayLink) {
        // `timestamp`: the refresh that just happened, i.e. what is on screen now.
        let t = l.timestamp
        autoreleasepool { onRefresh(t) }
    }
}
#endif

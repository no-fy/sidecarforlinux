#if canImport(UIKit)
import AVFoundation
import SidecarNet
import SidecarProtocol
import UIKit

/// Shows the PC's virtual display (phase 2). Frames go straight into an
/// AVSampleBufferDisplayLayer marked "display immediately": no pacing, no
/// extra buffering. They are enqueued by `sink`, on the network queue.
final class VideoView: UIView {
    override class var layerClass: AnyClass { AVSampleBufferDisplayLayer.self }
    private var displayLayer: AVSampleBufferDisplayLayer { layer as! AVSampleBufferDisplayLayer }
    /// Handed to `PeerServer.videoSink`.
    private(set) var sink: VideoSink!

    override init(frame: CGRect) {
        super.init(frame: frame)
        backgroundColor = .black
        displayLayer.videoGravity = .resizeAspect
        isUserInteractionEnabled = false
        sink = VideoSink(renderer: displayLayer.sampleBufferRenderer)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }
}

/// Queues decoded-on-display H.264 frames on the layer's renderer. Used only
/// on `PeerServer.queue` (serial), so no locking.
final class VideoSink: VideoFrameSink {
    private let renderer: AVSampleBufferVideoRenderer
    private var format: VideoFormat?

    init(renderer: AVSampleBufferVideoRenderer) {
        self.renderer = renderer
    }

    func configure(_ config: VideoConfig) {
        format = try? VideoFormat(config)
    }

    func enqueue(_ frame: VideoFrame) -> Bool {
        guard let format, let sample = try? format.sampleBuffer(frame) else { return false }
        if renderer.status == .failed {
            renderer.flush()
        }
        renderer.enqueue(sample)
        return renderer.status != .failed
    }

    func reset() {
        format = nil
        renderer.flush(removingDisplayedImage: true, completionHandler: nil)
    }
}
#endif

#if canImport(UIKit)
import AVFoundation
import SidecarNet
import SidecarProtocol
import UIKit

/// Video renderer "AVSampleBufferDisplayLayer": shows the PC's virtual
/// display (phase 2). Frames go straight into an AVSampleBufferDisplayLayer
/// marked "display immediately": no pacing, no extra buffering. They are
/// enqueued by `sink`, on the network queue.
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

/// Queues H.264 samples on the layer's renderer, which decodes and displays
/// them. Used only on `PeerServer.queue` (serial), so no locking.
final class VideoSink: VideoFrameSink {
    private let renderer: AVSampleBufferVideoRenderer
    private var format: VideoFormat?
    private var needKeyframe = true

    init(renderer: AVSampleBufferVideoRenderer) {
        self.renderer = renderer
    }

    func configure(_ config: VideoConfig) {
        format = try? VideoFormat(config)
        needKeyframe = true
    }

    func enqueue(_ frame: VideoFrame, receivedNs: UInt64, report: @escaping (VideoFeedback) -> Void) {
        var queued = false
        // After a renderer switch or a new format, P-frames would only show
        // garbage until the next keyframe.
        if !needKeyframe || frame.keyframe, let format, let sample = try? format.sampleBuffer(frame) {
            if renderer.status == .failed {
                renderer.flush()
            }
            renderer.enqueue(sample)
            queued = renderer.status != .failed
            needKeyframe = !queued
        }
        // The layer decodes and displays inside the system and tells nobody
        // when a given frame was decoded or reached the screen, so those
        // stay 0 ("unknown"). The Metal renderer measures them.
        report(VideoFeedback(frameID: frame.frameID, requestKeyframe: !queued, receivedNs: receivedNs,
                             decodedNs: 0, displayedNs: 0))
    }

    func reset() {
        format = nil
        needKeyframe = true
        renderer.flush(removingDisplayedImage: true, completionHandler: nil)
    }
}
#endif

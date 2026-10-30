#if canImport(UIKit)
import AVFoundation
import SidecarNet
import SidecarProtocol
import UIKit

/// Shows the PC's virtual display (phase 2). Frames go straight into an
/// AVSampleBufferDisplayLayer marked "display immediately": no pacing, no
/// extra buffering, lowest latency.
final class VideoView: UIView {
    override class var layerClass: AnyClass { AVSampleBufferDisplayLayer.self }
    private var displayLayer: AVSampleBufferDisplayLayer { layer as! AVSampleBufferDisplayLayer }
    private var format: VideoFormat?
    private(set) var framesShown = 0

    override init(frame: CGRect) {
        super.init(frame: frame)
        backgroundColor = .black
        displayLayer.videoGravity = .resizeAspect
        isUserInteractionEnabled = false
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    var isActive: Bool { format != nil }

    func configure(_ config: VideoConfig) {
        do {
            format = try VideoFormat(config)
        } catch {
            format = nil
        }
    }

    /// Returns false if the frame couldn't be shown (caller may ask for a keyframe).
    @discardableResult
    func enqueue(_ frame: VideoFrame) -> Bool {
        guard let format, let sample = try? format.sampleBuffer(frame) else { return false }
        let renderer = displayLayer.sampleBufferRenderer
        if renderer.status == .failed {
            renderer.flush()
        }
        renderer.enqueue(sample)
        framesShown += 1
        return renderer.status != .failed
    }

    func reset() {
        format = nil
        framesShown = 0
        displayLayer.sampleBufferRenderer.flush(removingDisplayedImage: true, completionHandler: nil)
    }
}
#endif

import Foundation
import SidecarProtocol

/// Sends the video channel to one of several renderers: the A/B switch in
/// the iPad's menu. `select` may be called from any thread and takes effect
/// at the next video message: the old renderer is reset, the new one gets
/// the current parameter sets again and waits for the next keyframe (the
/// daemon's encoders send one every 2 s).
public final class VideoSinkSwitch: VideoFrameSink, @unchecked Sendable {
    private let sinks: [VideoFrameSink]
    private let lock = NSLock()
    private var wanted: Int
    // PeerServer.queue only.
    private var active: Int
    private var config: VideoConfig?

    public init(_ sinks: [VideoFrameSink], selected: Int) {
        precondition(sinks.indices.contains(selected))
        self.sinks = sinks
        wanted = selected
        active = selected
    }

    public var selected: Int {
        lock.lock()
        defer { lock.unlock() }
        return wanted
    }

    public func select(_ index: Int) {
        precondition(sinks.indices.contains(index))
        lock.lock()
        wanted = index
        lock.unlock()
    }

    public func configure(_ config: VideoConfig) {
        switchIfNeeded()
        self.config = config
        sinks[active].configure(config)
    }

    public func enqueue(_ frame: VideoFrame, receivedNs: UInt64, report: @escaping (VideoFeedback) -> Void) {
        switchIfNeeded()
        sinks[active].enqueue(frame, receivedNs: receivedNs, report: report)
    }

    public func reset() {
        switchIfNeeded()
        config = nil
        sinks[active].reset()
    }

    private func switchIfNeeded() {
        let w = selected
        guard w != active else { return }
        sinks[active].reset()
        active = w
        if let config { sinks[w].configure(config) }
    }
}

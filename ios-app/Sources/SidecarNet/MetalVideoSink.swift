// Phase 2, video renderer "Metal": VideoToolbox decode → newest frame only
// → drawn into a CAMetalLayer and presented as soon as the GPU is done, with
// no display-link pacing. Unlike AVSampleBufferDisplayLayer it can report,
// per frame, when it was decoded and when it actually reached the screen
// (the drawable's presentedTime), so the PC can log where the time goes.
import CoreVideo
import Foundation
import Metal
import QuartzCore
import SidecarProtocol

/// `VideoFrameSink` calls come from PeerServer.queue. Drawing happens on a
/// dedicated thread, because `nextDrawable` blocks until the display hands a
/// drawable back.
public final class MetalVideoSink: VideoFrameSink, @unchecked Sendable {
    public let layer: CAMetalLayer
    private let device: MTLDevice?
    private let onError: (String) -> Void
    private var decoder: LowLatencyDecoder!
    private let slot = LatestSlot<DecodedVideoFrame>()
    private let lock = NSLock()
    private var paused = false
    private var lastError: String?
    // Render thread only.
    private var renderer: VideoRenderer?
    private var rendererFailed = false

    /// Call on the main thread (it configures `layer`). `onError` runs on
    /// any thread, once per distinct problem.
    public init(layer: CAMetalLayer, device: MTLDevice? = MTLCreateSystemDefaultDevice(),
                onError: @escaping (String) -> Void = { _ in }) {
        self.layer = layer
        self.device = device
        self.onError = onError
        layer.device = device
        layer.pixelFormat = .bgra8Unorm
        layer.framebufferOnly = true
        // Two drawables: one on screen, one being drawn. A third would only
        // let frames queue up behind each other.
        layer.maximumDrawableCount = 2
        layer.presentsWithTransaction = false
        layer.isOpaque = true
        decoder = LowLatencyDecoder { [weak self] frame in
            guard let self else { return frame.finish(displayedNs: 0) }
            self.decoded(frame)
        }
        let t = Thread { [weak self] in self?.renderLoop() }
        t.name = "sidecar.video.metal"
        t.qualityOfService = .userInteractive
        t.start()
    }

    /// True while the app is in the background, where iOS forbids GPU work:
    /// frames are dropped instead of drawn.
    public var isPaused: Bool {
        get {
            lock.lock()
            defer { lock.unlock() }
            return paused
        }
        set {
            lock.lock()
            paused = newValue
            lock.unlock()
            if newValue { slot.poll()?.finish(displayedNs: 0) }
        }
    }

    /// Ends the render thread (tests; the app keeps its sink for its lifetime).
    public func stop() {
        slot.close()
        slot.poll()?.finish(displayedNs: 0)
    }

    // MARK: VideoFrameSink (PeerServer.queue)

    public func configure(_ config: VideoConfig) {
        do {
            try decoder.configure(config)
        } catch {
            notify("can't decode this video format: \(error)")
        }
    }

    public func enqueue(_ frame: VideoFrame, receivedNs: UInt64, report: @escaping (VideoFeedback) -> Void) {
        decoder.decode(frame, receivedNs: receivedNs, report: report)
    }

    public func reset() {
        decoder.invalidate()
        slot.poll()?.finish(displayedNs: 0)
    }

    // MARK: internals

    /// VideoToolbox thread.
    private func decoded(_ frame: DecodedVideoFrame) {
        if isPaused { return frame.finish(displayedNs: 0) }
        // A frame still waiting here was never drawn: the newer one wins.
        slot.put(frame)?.finish(displayedNs: 0)
    }

    private func renderLoop() {
        _ = makeRendererIfNeeded()  // compile the shader before the first frame arrives
        while let frame = slot.take() {
            autoreleasepool { draw(frame) }
        }
    }

    private func draw(_ first: DecodedVideoFrame) {
        var frame = first
        guard !isPaused, let renderer = makeRendererIfNeeded() else { return frame.finish(displayedNs: 0) }
        // Waits while both drawables are in use (up to one display refresh).
        guard let drawable = layer.nextDrawable() else {
            notify("no drawable (is the video view on screen?)")
            return frame.finish(displayedNs: 0)
        }
        // A newer frame may have been decoded while we waited: show that one.
        if let newer = slot.poll() {
            frame.finish(displayedNs: 0)
            frame = newer
        }
        guard !isPaused else { return frame.finish(displayedNs: 0) }
        do {
            let cb = try renderer.encode(frame.image, into: drawable.texture)
            let shown = frame
            drawable.addPresentedHandler { d in
                // presentedTime is 0 if the drawable never made it to the screen.
                let t = d.presentedTime
                shown.finish(displayedNs: t > 0 ? SessionClock.ns(fromMediaTime: t) : 0)
            }
            cb.present(drawable)
            cb.commit()
        } catch {
            notify("can't draw the frame: \(error)")
            frame.finish(displayedNs: 0)
        }
    }

    private func makeRendererIfNeeded() -> VideoRenderer? {
        if let renderer { return renderer }
        if rendererFailed { return nil }
        guard let device else {
            rendererFailed = true
            notify("this device has no Metal GPU")
            return nil
        }
        do {
            renderer = try VideoRenderer(device: device, pixelFormat: layer.pixelFormat)
        } catch {
            rendererFailed = true
            notify("Metal setup failed: \(error)")
        }
        return renderer
    }

    private func notify(_ message: String) {
        lock.lock()
        let isNew = message != lastError
        lastError = message
        lock.unlock()
        if isNew { onError(message) }
    }
}

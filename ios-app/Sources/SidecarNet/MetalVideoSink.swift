// Phase 2, video renderer "Metal": VideoToolbox decode → newest frame only
// → drawn into a CAMetalLayer. Unlike AVSampleBufferDisplayLayer it can
// report, per frame, when it was decoded and when it actually reached the
// screen (the drawable's presentedTime), so the PC can log where the time
// goes.
//
// How a frame is handed to the screen is selectable (`MetalPresentMode`),
// because that's where most of the iPad's latency is: Core Animation's
// render server composites on its own vsync-paced schedule ("Understanding
// hitches in your app", developer.apple.com). Round 1 presented every frame
// at once and measured ~40 ms decoded → on screen on an iPad Air M4: with
// ~110 fps arriving at a 60 Hz panel, each drawable queued behind the
// previous one (DECISIONS D21).
import CoreVideo
import Foundation
import Metal
import QuartzCore
import SidecarProtocol

public enum MetalPresentMode: String, CaseIterable, Sendable {
    /// `CAMetalDisplayLink` with `preferredFrameLatency = 1` on its own
    /// thread: once per refresh, the newest decoded frame is drawn into the
    /// drawable the link hands out. Nothing queues behind anything.
    case displayLink
    /// iOS 18+ `UIUpdateLink` with `wantsImmediatePresentation`: drawn on the
    /// main thread inside the UI update and presented with its Core Animation
    /// transaction, which Apple says then "presents one frame duration
    /// sooner". The host view drives it through `presentForUIUpdate`.
    case uiUpdate
    /// Round 1: draw and present each frame as soon as it is decoded.
    case immediate

    public var label: String {
        switch self {
        case .displayLink: return "display link"
        case .uiUpdate: return "UI update link"
        case .immediate: return "present at once"
        }
    }
}

/// `VideoFrameSink` calls come from PeerServer.queue. Drawing happens on the
/// display link's thread, on the main thread (UI update link) or on a render
/// thread (present at once), depending on `mode`.
public final class MetalVideoSink: VideoFrameSink, @unchecked Sendable {
    public let layer: CAMetalLayer
    private let device: MTLDevice?
    private let onError: (String) -> Void
    private let onStats: (String) -> Void
    private var decoder: LowLatencyDecoder!
    /// Frames for the `.immediate` render thread.
    private let immediateSlot = LatestSlot<DecodedVideoFrame>()
    /// Frames for the paced modes, picked up once per refresh.
    private let pacedSlot = LatestSlot<DecodedVideoFrame>()
    private var displayLink: DisplayLinkThread?
    private var active = true  // main thread only
    // Guarded by `lock`.
    private let lock = NSLock()
    private var paused = false
    private var currentMode: MetalPresentMode
    private var inFlight = 0
    private var lastPresentNs: UInt64 = 0
    private var lastError: String?
    private var window = PresentWindow()
    private var windowStartNs: UInt64 = 0
    /// Held while the layer is reconfigured (mode switch, display link on or
    /// off) and around manual `nextDrawable()` calls, so a drawable is never
    /// taken by hand while a display link owns the layer.
    private let layerLock = NSLock()
    // Guarded by `drawLock`: one frame is drawn at a time, whichever thread.
    private let drawLock = NSLock()
    private var renderer: VideoRenderer?
    private var rendererFailed = false

    /// Call on the main thread (it configures `layer`). `onError` and
    /// `onStats` (about once a second while frames are shown) run on any
    /// thread.
    public init(layer: CAMetalLayer, device: MTLDevice? = MTLCreateSystemDefaultDevice(),
                mode: MetalPresentMode = .displayLink,
                onError: @escaping (String) -> Void = { _ in },
                onStats: @escaping (String) -> Void = { _ in }) {
        self.layer = layer
        self.device = device
        self.onError = onError
        self.onStats = onStats
        currentMode = mode
        layer.device = device
        layer.pixelFormat = .bgra8Unorm
        layer.framebufferOnly = true
        layer.isOpaque = true
        decoder = LowLatencyDecoder { [weak self] frame in
            guard let self else { return frame.finish(displayedNs: 0) }
            self.decoded(frame)
        }
        let t = Thread { [weak self] in self?.immediateLoop() }
        t.name = "sidecar.video.metal"
        t.qualityOfService = .userInteractive
        t.start()
        applyMode(mode)
    }

    /// Call on the main thread (layer properties change with the mode).
    public func setMode(_ mode: MetalPresentMode) {
        lock.lock()
        let changed = mode != currentMode
        currentMode = mode
        inFlight = 0
        window = PresentWindow()
        windowStartNs = 0
        lock.unlock()
        if changed { applyMode(mode) }
    }

    /// Call on the main thread: false while the video view is hidden, so the
    /// display link doesn't tick for nothing.
    public func setActive(_ on: Bool) {
        guard on != active else { return }
        active = on
        layerLock.lock()
        updateDisplayLink()
        layerLock.unlock()
    }

    public var mode: MetalPresentMode {
        lock.lock()
        defer { lock.unlock() }
        return currentMode
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
            if newValue { drainSlots() }
        }
    }

    /// Ends the threads (tests; the app keeps its sink for its lifetime).
    public func stop() {
        displayLink?.stop()
        immediateSlot.close()
        drainSlots()
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
        drainSlots()
    }

    // MARK: .uiUpdate (main thread)

    /// Call from the UI update, before its Core Animation commit. `target`:
    /// the update's estimated presentation time (media time);
    /// `immediateExpected`: what UIUpdateInfo says about this update.
    public func presentForUIUpdate(target: CFTimeInterval?, immediateExpected: Bool) {
        guard mode == .uiUpdate, !isPaused, canPresentWithoutBlocking(), let frame = pacedSlot.poll() else { return }
        layerLock.lock()
        let drawable = mode == .uiUpdate ? layer.nextDrawable() : nil
        layerLock.unlock()
        guard let drawable else {
            notify("no drawable (is the video view on screen?)")
            return frame.finish(displayedNs: 0)
        }
        present(frame, drawable: drawable, mode: .uiUpdate, target: target, immediateExpected: immediateExpected)
    }

    // MARK: internals

    private func applyMode(_ mode: MetalPresentMode) {
        layerLock.lock()
        defer { layerLock.unlock() }
        // CAMetalLayer throws (CAMetalLayerInvalidOperation) if its drawable
        // count changes while a CAMetalDisplayLink drives it: detach the link
        // first, and wait until it's gone.
        displayLink?.stop()
        // Round 1 used two drawables: one on screen, one being drawn. The
        // paced modes draw at most one frame per refresh, so a third drawable
        // can't queue frames; it only keeps the link from stalling.
        layer.maximumDrawableCount = mode == .immediate ? 2 : 3
        // UIUpdateLink: the drawable goes out with the UI update's transaction.
        layer.presentsWithTransaction = mode == .uiUpdate
        updateDisplayLink()
        drainSlots()
    }

    private func updateDisplayLink() {
        if mode == .displayLink && active {
            if displayLink == nil {
                displayLink = DisplayLinkThread { [weak self] update in self?.displayLinkUpdate(update) }
            }
            displayLink?.start(layer: layer)
        } else {
            displayLink?.stop()
        }
    }

    /// VideoToolbox thread.
    private func decoded(_ frame: DecodedVideoFrame) {
        lock.lock()
        let (m, p) = (currentMode, paused)
        lock.unlock()
        if p { return frame.finish(displayedNs: 0) }
        // A frame still waiting was never drawn: the newer one wins.
        (m == .immediate ? immediateSlot : pacedSlot).put(frame)?.finish(displayedNs: 0)
    }

    private func drainSlots() {
        immediateSlot.poll()?.finish(displayedNs: 0)
        pacedSlot.poll()?.finish(displayedNs: 0)
    }

    /// Display link thread, once per refresh.
    private func displayLinkUpdate(_ update: CAMetalDisplayLink.Update) {
        // No new frame: leave the drawable alone (the screen keeps the last one).
        guard mode == .displayLink, !isPaused, let frame = pacedSlot.poll() else { return }
        present(frame, drawable: update.drawable, mode: .displayLink, target: update.targetPresentationTimestamp,
                immediateExpected: false)
    }

    private func immediateLoop() {
        drawLock.lock()
        _ = rendererLocked()  // compile the shader before the first frame arrives
        drawLock.unlock()
        while var frame = immediateSlot.take() {
            autoreleasepool {
                guard mode == .immediate, !isPaused else { return frame.finish(displayedNs: 0) }
                // Waits while both drawables are in use (up to one display refresh).
                layerLock.lock()
                let drawable = mode == .immediate ? layer.nextDrawable() : nil
                layerLock.unlock()
                guard let drawable else {
                    notify("no drawable (is the video view on screen?)")
                    return frame.finish(displayedNs: 0)
                }
                // A newer frame may have been decoded while we waited: show that one.
                if let newer = immediateSlot.poll() {
                    frame.finish(displayedNs: 0)
                    frame = newer
                }
                present(frame, drawable: drawable, mode: .immediate, target: nil, immediateExpected: false)
            }
        }
    }

    private func present(_ frame: DecodedVideoFrame, drawable: CAMetalDrawable, mode: MetalPresentMode,
                         target: CFTimeInterval?, immediateExpected: Bool) {
        drawLock.lock()
        defer { drawLock.unlock() }
        guard !isPaused, let renderer = rendererLocked() else { return frame.finish(displayedNs: 0) }
        do {
            let cb = try renderer.encode(frame.image, into: drawable.texture)
            let submittedNs = SessionClock.nowNs()
            let targetNs = target.map { SessionClock.ns(fromMediaTime: $0) }
            lock.lock()
            inFlight += 1
            lastPresentNs = submittedNs
            lock.unlock()
            drawable.addPresentedHandler { [weak self] d in
                // presentedTime is 0 if the drawable never made it to the screen.
                let t = d.presentedTime
                let shownNs = t > 0 ? SessionClock.ns(fromMediaTime: t) : 0
                frame.finish(displayedNs: shownNs)
                self?.presented(mode: mode, decodedNs: frame.decodedNs, submittedNs: submittedNs,
                                displayedNs: shownNs, targetNs: targetNs, immediateExpected: immediateExpected)
            }
            if mode == .uiUpdate {
                // presentsWithTransaction: Apple's order is commit, wait until
                // scheduled, then present inside the current transaction.
                cb.commit()
                cb.waitUntilScheduled()
                drawable.present()
            } else {
                cb.present(drawable)
                cb.commit()
            }
        } catch {
            notify("can't draw the frame: \(error)")
            frame.finish(displayedNs: 0)
        }
    }

    /// Presented handler (any thread).
    private func presented(mode: MetalPresentMode, decodedNs: UInt64, submittedNs: UInt64, displayedNs: UInt64,
                           targetNs: UInt64?, immediateExpected: Bool) {
        let now = SessionClock.nowNs()
        var summary: String?
        lock.lock()
        inFlight = max(0, inFlight - 1)
        if mode == currentMode {
            window.add(decodedNs: decodedNs, submittedNs: submittedNs, displayedNs: displayedNs, targetNs: targetNs,
                       immediateExpected: immediateExpected)
            if windowStartNs == 0 { windowStartNs = now }
            let seconds = Double(now - windowStartNs) / 1e9
            if seconds >= 1, let s = window.summary(mode: mode, seconds: seconds) {
                summary = s
                window = PresentWindow()
                windowStartNs = now
            }
        }
        lock.unlock()
        if let summary { onStats(summary) }
    }

    /// The main thread must never wait for a drawable: skip this update if
    /// two are still on their way to the screen. A presented handler that
    /// never comes (shouldn't happen) unblocks after 250 ms.
    private func canPresentWithoutBlocking() -> Bool {
        lock.lock()
        defer { lock.unlock() }
        if inFlight >= 2 && SessionClock.nowNs() > lastPresentNs + 250_000_000 { inFlight = 0 }
        return inFlight < 2
    }

    private func rendererLocked() -> VideoRenderer? {
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

/// Runs a CAMetalDisplayLink on its own thread's run loop, away from the
/// main thread (busy with 240 Hz pencil touches).
final class DisplayLinkThread: NSObject, CAMetalDisplayLinkDelegate, @unchecked Sendable {
    private let onUpdate: (CAMetalDisplayLink.Update) -> Void
    private var runLoop: CFRunLoop?
    private var link: CAMetalDisplayLink?  // run loop thread only

    init(onUpdate: @escaping (CAMetalDisplayLink.Update) -> Void) {
        self.onUpdate = onUpdate
        super.init()
        let ready = DispatchSemaphore(value: 0)
        let t = Thread { [self] in
            runLoop = CFRunLoopGetCurrent()
            RunLoop.current.add(NSMachPort(), forMode: .default)  // keeps the run loop alive
            ready.signal()
            while true { _ = RunLoop.current.run(mode: .default, before: .distantFuture) }
        }
        t.name = "sidecar.video.displaylink"
        t.qualityOfService = .userInteractive
        t.start()
        ready.wait()
    }

    /// Both block until done on the link's thread, so the layer can be
    /// reconfigured safely afterwards. Call from the main thread.
    func start(layer: CAMetalLayer) {
        performAndWait {
            guard self.link == nil else { return }
            let l = CAMetalDisplayLink(metalLayer: layer)
            // Ask for the shortest render window Core Animation offers (1 or 2 frames).
            l.preferredFrameLatency = 1
            l.delegate = self
            l.add(to: .current, forMode: .default)
            self.link = l
        }
    }

    func stop() {
        performAndWait {
            self.link?.invalidate()
            self.link = nil
        }
    }

    private func performAndWait(_ f: @escaping () -> Void) {
        guard let runLoop else { return }
        let done = DispatchSemaphore(value: 0)
        CFRunLoopPerformBlock(runLoop, CFRunLoopMode.defaultMode.rawValue) {
            f()
            done.signal()
        }
        CFRunLoopWakeUp(runLoop)
        done.wait()
    }

    func metalDisplayLink(_ link: CAMetalDisplayLink, needsUpdate update: CAMetalDisplayLink.Update) {
        // Drawables are autoreleased: release them every refresh, or the
        // layer runs out.
        autoreleasepool { onUpdate(update) }
    }
}

/// One second of presented frames for the on-iPad readout.
public struct PresentWindow {
    private var decodedToScreenUs: [UInt32] = []
    private var submitToScreenUs: [UInt32] = []
    public private(set) var dropped = 0
    private var withTarget = 0
    private var onTarget = 0
    private var immediate = 0

    public init() {}

    public var shown: Int { decodedToScreenUs.count }

    /// `targetNs`: when the presenter expected the frame on screen. "On
    /// target" = shown no later than 4 ms after that.
    public mutating func add(decodedNs: UInt64, submittedNs: UInt64, displayedNs: UInt64, targetNs: UInt64?,
                             immediateExpected: Bool) {
        guard displayedNs != 0 else {
            dropped += 1
            return
        }
        decodedToScreenUs.append(Self.us(displayedNs, decodedNs))
        submitToScreenUs.append(Self.us(displayedNs, submittedNs))
        if let targetNs {
            withTarget += 1
            if displayedNs <= targetNs + 4_000_000 { onTarget += 1 }
        }
        if immediateExpected { immediate += 1 }
    }

    /// e.g. "Metal · display link: decoded → screen 24.1/31.0 ms · submit → screen 16.9 ms · 59 shown/s · on target 100%"
    public func summary(mode: MetalPresentMode, seconds: Double) -> String? {
        guard shown > 0 || dropped > 0 else { return nil }
        func ms(_ us: UInt32) -> String { String(format: "%.1f", Double(us) / 1000) }
        let d = decodedToScreenUs.sorted(), s = submitToScreenUs.sorted()
        var parts = ["Metal · \(mode.label): decoded → screen \(ms(Self.pct(d, 50)))/\(ms(Self.pct(d, 95))) ms",
                     "submit → screen \(ms(Self.pct(s, 50))) ms",
                     String(format: "%.0f shown/s", Double(shown) / max(seconds, 0.001))]
        if dropped > 0 { parts.append("\(dropped) dropped") }
        if withTarget > 0 { parts.append("on target \(onTarget * 100 / withTarget)%") }
        if mode == .uiUpdate && shown > 0 { parts.append("immediate \(immediate * 100 / shown)%") }
        return parts.joined(separator: " · ")
    }

    private static func us(_ a: UInt64, _ b: UInt64) -> UInt32 {
        a > b ? UInt32(min((a - b) / 1000, UInt64(UInt32.max))) : 0
    }

    private static func pct(_ sorted: [UInt32], _ p: Double) -> UInt32 {
        guard !sorted.isEmpty else { return 0 }
        return sorted[min(sorted.count - 1, Int((p / 100 * Double(sorted.count - 1)).rounded()))]
    }
}

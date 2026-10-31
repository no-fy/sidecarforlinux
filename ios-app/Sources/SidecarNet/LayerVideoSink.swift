// Video renderer "AVSampleBufferDisplayLayer", with display timing.
//
// The layer decodes and displays inside iPadOS and has no per-frame
// callback. What it does offer (iOS 17.4+) is `displayedPixelBuffer()`: a
// copy of the picture it shows now. Polled once per display refresh, that
// tells *when* the picture changed (±1 refresh). To tell *which* frame it
// is, the app also decodes every frame itself (VideoToolbox, the Metal
// renderer's decoder, output only fingerprinted, never shown): H.264 decoding
// is bit-exact by specification, so the shown picture's luma matches exactly
// one decoded frame. Custom attachments don't survive into the copy, so they
// can't identify frames.
//
// "decoded" stays unknowable here (it happens inside the layer), so
// VideoFeedback.decoded_ns stays 0 and displayed_ns carries the refresh at
// which the new picture was first seen.
import AVFoundation
import CoreVideo
import Foundation
import SidecarProtocol

/// A hash of a picture's first plane (luma for NV12), row by row without
/// the row padding, plus its size and format. Two pictures with the same
/// fingerprint have identical luma (up to hash collisions).
public enum LumaFingerprint {
    public static func of(_ image: CVPixelBuffer) -> UInt64? {
        guard CVPixelBufferLockBaseAddress(image, .readOnly) == kCVReturnSuccess else { return nil }
        defer { CVPixelBufferUnlockBaseAddress(image, .readOnly) }
        let format = CVPixelBufferGetPixelFormatType(image)
        let planar = CVPixelBufferIsPlanar(image)
        guard let base = planar ? CVPixelBufferGetBaseAddressOfPlane(image, 0) : CVPixelBufferGetBaseAddress(image)
        else { return nil }
        let width = planar ? CVPixelBufferGetWidthOfPlane(image, 0) : CVPixelBufferGetWidth(image)
        let height = planar ? CVPixelBufferGetHeightOfPlane(image, 0) : CVPixelBufferGetHeight(image)
        let stride = planar ? CVPixelBufferGetBytesPerRowOfPlane(image, 0) : CVPixelBufferGetBytesPerRow(image)
        // 8-bit luma for the 4:2:0 formats, 4 bytes for packed RGB(A); else
        // the whole row, padding included.
        let rowBytes: Int
        switch format {
        case kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange, kCVPixelFormatType_420YpCbCr8BiPlanarFullRange:
            rowBytes = width
        case kCVPixelFormatType_32BGRA, kCVPixelFormatType_32ARGB, kCVPixelFormatType_32RGBA:
            rowBytes = width * 4
        default:
            rowBytes = stride
        }
        let p = UnsafeRawPointer(base)
        let k: UInt64 = 0x9E37_79B9_7F4A_7C15
        var h0: UInt64 = 0x243F_6A88_85A3_08D3, h1: UInt64 = 0x1319_8A2E_0370_7344
        var h2: UInt64 = 0xA409_3822_299F_31D0, h3: UInt64 = 0x082E_FA98_EC4E_6C89
        for y in 0..<height {
            let row = p.advanced(by: y * stride)
            var x = 0
            while x + 32 <= rowBytes {
                h0 = (h0 ^ row.loadUnaligned(fromByteOffset: x, as: UInt64.self)) &* k
                h1 = (h1 ^ row.loadUnaligned(fromByteOffset: x + 8, as: UInt64.self)) &* k
                h2 = (h2 ^ row.loadUnaligned(fromByteOffset: x + 16, as: UInt64.self)) &* k
                h3 = (h3 ^ row.loadUnaligned(fromByteOffset: x + 24, as: UInt64.self)) &* k
                x += 32
            }
            while x < rowBytes {
                h0 = (h0 ^ UInt64(row.load(fromByteOffset: x, as: UInt8.self))) &* k
                x += 1
            }
            h1 ^= h1 >> 29
            h2 ^= h2 >> 31
        }
        var h = UInt64(width) &* 31 &+ UInt64(height) &* 37 &+ UInt64(format)
        for lane in [h0, h1, h2, h3] {
            h = (h ^ lane ^ (lane >> 32)) &* k
        }
        return h
    }
}

/// Works out when each enqueued frame appeared on screen from what the
/// layer shows at each refresh. Every frame handed to `enqueued` gets
/// exactly one report: its display time, or 0 if it was never seen
/// (replaced by a newer frame before a refresh, identical to what was
/// already shown, or older than `maxAgeNs`). Thread-safe.
public final class DisplayMatcher: @unchecked Sendable {
    public enum Match: Equatable { case exact, afterPause }

    /// A frame counts as "after a pause" when it came at least this long
    /// after the previous one (the daemon's pen → frame probe uses the same
    /// idea).
    public static let pauseNs: UInt64 = 150_000_000
    public static let maxAgeNs: UInt64 = 500_000_000

    private struct Pending {
        let frameID: UInt32
        let receivedNs: UInt64
        var fingerprint: UInt64?
        var fingerprintKnown = false
        let afterPause: Bool
        let report: (VideoFeedback) -> Void
    }

    private typealias Out = [(report: (VideoFeedback) -> Void, feedback: VideoFeedback)]

    private let lock = NSLock()
    private var pending: [Pending] = []
    private var lastReceivedNs: UInt64 = 0
    private var shown: UInt64?
    /// A screen change whose frame isn't fingerprinted yet.
    private var unresolved: (fingerprint: UInt64, atNs: UInt64)?
    private var window = LayerWindow()

    public init() {}

    public func enqueued(frameID: UInt32, receivedNs: UInt64, report: @escaping (VideoFeedback) -> Void) {
        lock.lock()
        let gap = lastReceivedNs == 0 || receivedNs >= lastReceivedNs + Self.pauseNs
        lastReceivedNs = receivedNs
        pending.append(Pending(frameID: frameID, receivedNs: receivedNs, afterPause: gap, report: report))
        let out = expireLocked(nowNs: receivedNs)
        lock.unlock()
        Self.deliver(out)
    }

    /// The app's own decode of the frame (nil: couldn't decode or read it).
    public func fingerprinted(frameID: UInt32, fingerprint: UInt64?) {
        lock.lock()
        var out: Out = []
        if let i = pending.firstIndex(where: { $0.frameID == frameID }) {
            pending[i].fingerprint = fingerprint
            pending[i].fingerprintKnown = true
            if let u = unresolved { out = resolveLocked(u.fingerprint, atNs: u.atNs) }
        }
        lock.unlock()
        Self.deliver(out)
    }

    /// One display refresh: the fingerprint of the picture the layer shows
    /// (nil: it gave none), at `atNs` (session clock).
    public func observed(fingerprint: UInt64?, atNs: UInt64) {
        lock.lock()
        window.polls += 1
        var out: Out = []
        if let fp = fingerprint {
            if fp != shown {
                shown = fp
                window.changes += 1
                out = resolveLocked(fp, atNs: atNs)
            }
        } else {
            window.nilPolls += 1
        }
        out += expireLocked(nowNs: atNs)
        lock.unlock()
        Self.deliver(out)
    }

    /// Video stopped or measuring turned off: report everything still open.
    public func flush() {
        lock.lock()
        let out: Out = pending.map { (report: $0.report, feedback: Self.feedback($0, displayedNs: 0)) }
        pending.removeAll()
        unresolved = nil
        shown = nil
        lastReceivedNs = 0
        lock.unlock()
        Self.deliver(out)
    }

    /// The statistics since the last call.
    public func takeWindow() -> LayerWindow {
        lock.lock()
        defer { lock.unlock() }
        let w = window
        window = LayerWindow()
        return w
    }

    // MARK: internals (lock held)

    private func resolveLocked(_ fp: UInt64, atNs: UInt64) -> Out {
        // Frames enqueued before this refresh, oldest first. The layer shows
        // them in order, so the match is the oldest one with this picture;
        // an older frame not fingerprinted yet might be it, so wait for it.
        let candidates = pending.indices.filter { pending[$0].receivedNs <= atNs }
        if candidates.isEmpty {
            unresolved = nil  // e.g. the picture already up when measuring started
            return []
        }
        for i in candidates {
            if !pending[i].fingerprintKnown {
                unresolved = (fp, atNs)
                return []
            }
            if pending[i].fingerprint == fp {
                unresolved = nil
                return settleLocked(upTo: i, atNs: atNs, match: .exact)
            }
        }
        unresolved = nil
        window.unmatched += 1
        // No decoded frame looks like this (e.g. the copy isn't bit-identical
        // on this device): only the first frame after a pause can be paired
        // with the change without ambiguity.
        if let i = candidates.first(where: { pending[$0].afterPause }) {
            return settleLocked(upTo: i, atNs: atNs, match: .afterPause)
        }
        return []
    }

    /// Frame `i` reached the screen at `atNs`; older open frames never did.
    private func settleLocked(upTo i: Int, atNs: UInt64, match: Match) -> Out {
        var out: Out = []
        for j in 0...i {
            let p = pending[j]
            out.append((report: p.report, feedback: Self.feedback(p, displayedNs: j == i ? atNs : 0)))
        }
        let shownFrame = pending[i]
        window.add(receivedToScreenNs: atNs > shownFrame.receivedNs ? atNs - shownFrame.receivedNs : 0, match: match)
        pending.removeFirst(i + 1)
        return out
    }

    private func expireLocked(nowNs: UInt64) -> Out {
        var out: Out = []
        while let p = pending.first, nowNs > p.receivedNs + Self.maxAgeNs {
            out.append((report: p.report, feedback: Self.feedback(p, displayedNs: 0)))
            pending.removeFirst()
        }
        return out
    }

    private static func deliver(_ out: Out) {
        for o in out { o.report(o.feedback) }
    }

    private static func feedback(_ p: Pending, displayedNs: UInt64) -> VideoFeedback {
        VideoFeedback(frameID: p.frameID, requestKeyframe: false, receivedNs: p.receivedNs, decodedNs: 0,
                      displayedNs: displayedNs)
    }
}

/// One readout window of the AVSampleBufferDisplayLayer renderer.
public struct LayerWindow {
    public var polls = 0
    public var nilPolls = 0
    public var changes = 0
    public var unmatched = 0
    public private(set) var exactUs: [UInt32] = []
    public private(set) var afterPauseUs: [UInt32] = []

    public init() {}

    mutating func add(receivedToScreenNs ns: UInt64, match: DisplayMatcher.Match) {
        let us = UInt32(min(ns / 1000, UInt64(UInt32.max)))
        if match == .exact { exactUs.append(us) } else { afterPauseUs.append(us) }
    }

    /// e.g. "AVSampleBufferDisplayLayer: received → screen 41.0/52.3 ms (±1 refresh, 57 matched/s) · …"
    /// `extra`: the layer's own metrics and the copy's format, appended.
    public func summary(seconds: Double, extra: [String] = []) -> String? {
        guard polls > 0 else { return nil }
        func ms(_ us: UInt32) -> String { String(format: "%.1f", Double(us) / 1000) }
        func pct(_ v: [UInt32], _ p: Double) -> UInt32 {
            let s = v.sorted()
            return s.isEmpty ? 0 : s[min(s.count - 1, Int((p / 100 * Double(s.count - 1)).rounded()))]
        }
        var parts: [String]
        if exactUs.isEmpty && afterPauseUs.isEmpty {
            if nilPolls == polls {
                parts = ["AVSampleBufferDisplayLayer: not measurable, displayedPixelBuffer() gave nothing in \(polls) refreshes"]
            } else {
                parts = ["AVSampleBufferDisplayLayer: no frame matched yet (\(changes) screen changes)"]
            }
        } else {
            parts = ["AVSampleBufferDisplayLayer:"]
            if !exactUs.isEmpty {
                parts[0] += " received → screen \(ms(pct(exactUs, 50)))/\(ms(pct(exactUs, 95))) ms (±1 refresh)"
                parts.append(String(format: "%.0f matched/s", Double(exactUs.count) / max(seconds, 0.001)))
            }
            if !afterPauseUs.isEmpty {
                parts.append("after a pause \(ms(pct(afterPauseUs, 50))) ms (\(afterPauseUs.count))")
            }
            if unmatched > 0 { parts.append("\(unmatched) changes unmatched") }
        }
        if nilPolls > 0 && nilPolls < polls { parts.append("\(nilPolls)/\(polls) refreshes without a picture") }
        return (parts + extra).joined(separator: " · ")
    }
}

/// The renderer: queues H.264 samples on the layer's renderer, which
/// decodes and displays them, and (while `setMeasuring(true)`) works out
/// when each one appeared. `VideoFrameSink` calls come from PeerServer.queue;
/// `pollDisplayed` from a display-refresh thread.
public final class LayerVideoSink: VideoFrameSink, @unchecked Sendable {
    private let renderer: AVSampleBufferVideoRenderer
    private let onStats: (String) -> Void
    // PeerServer.queue only.
    private var format: VideoFormat?
    private var config: VideoConfig?
    private var needKeyframe = true
    private var decoder: LowLatencyDecoder!
    // Shared.
    private let matcher = DisplayMatcher()
    private let lock = NSLock()
    private var measuring = false
    /// Measuring (re)started: the fingerprint decoder has missed frames and
    /// must start again at a keyframe.
    private var restartDecoder = false
    private var windowStartNs: UInt64 = 0
    private var copyFormat = ""
    private var lastMetrics: Metrics?
    private var metricsText: String?

    /// `onStats`: about once a second while measuring, any thread.
    public init(renderer: AVSampleBufferVideoRenderer, onStats: @escaping (String) -> Void = { _ in }) {
        self.renderer = renderer
        self.onStats = onStats
        decoder = LowLatencyDecoder { [weak self] frame in
            // Fingerprint our own decode of the frame, then let it go.
            self?.matcher.fingerprinted(frameID: frame.frameID, fingerprint: LumaFingerprint.of(frame.image))
            frame.finish(displayedNs: 0)
        }
    }

    /// Any thread. On while the layer is on screen and polled; off reports
    /// every open frame as not shown and goes back to reporting at once.
    public func setMeasuring(_ on: Bool) {
        lock.lock()
        let changed = on != measuring
        measuring = on
        if changed && on { restartDecoder = true }
        windowStartNs = 0
        lock.unlock()
        if changed && !on { matcher.flush() }
    }

    private var isMeasuring: Bool {
        lock.lock()
        defer { lock.unlock() }
        return measuring
    }

    private func takeDecoderRestart() -> Bool {
        lock.lock()
        defer { lock.unlock() }
        let r = restartDecoder
        restartDecoder = false
        return r
    }

    // MARK: VideoFrameSink (PeerServer.queue)

    public func configure(_ config: VideoConfig) {
        format = try? VideoFormat(config)
        self.config = config
        needKeyframe = true
        try? decoder.configure(config)
    }

    public func enqueue(_ frame: VideoFrame, receivedNs: UInt64, report: @escaping (VideoFeedback) -> Void) {
        var sample: CMSampleBuffer?
        // After a renderer switch or a new format, P-frames would only show
        // garbage until the next keyframe.
        if !needKeyframe || frame.keyframe, let format {
            sample = try? format.sampleBuffer(frame)
        }
        guard let sample else { return report(Self.notQueued(frame, receivedNs)) }
        let measure = isMeasuring
        if measure {
            // Before the layer can possibly show it.
            matcher.enqueued(frameID: frame.frameID, receivedNs: receivedNs, report: report)
        }
        if renderer.status == .failed {
            renderer.flush()
        }
        renderer.enqueue(sample)
        let queued = renderer.status != .failed
        needKeyframe = !queued
        if !measure {
            // Not measuring: the layer tells nobody when a frame was decoded
            // or shown, so both stay 0 ("unknown").
            return report(queued ? VideoFeedback(frameID: frame.frameID, requestKeyframe: false, receivedNs: receivedNs,
                                                 decodedNs: 0, displayedNs: 0)
                                 : Self.notQueued(frame, receivedNs))
        }
        if takeDecoderRestart(), let config {
            decoder.invalidate()
            try? decoder.configure(config)  // a new session waits for a keyframe
        }
        decoder.decode(frame, receivedNs: receivedNs) { [weak self] fb in
            // Our decode failed (a decoded frame reports with decodedNs > 0).
            if fb.decodedNs == 0 { self?.matcher.fingerprinted(frameID: fb.frameID, fingerprint: nil) }
        }
    }

    public func reset() {
        format = nil
        config = nil
        needKeyframe = true
        renderer.flush(removingDisplayedImage: true, completionHandler: nil)
        decoder.invalidate()
        matcher.flush()
    }

    // MARK: display refresh

    /// Once per display refresh, `atNs` = that refresh (session clock).
    public func pollDisplayed(atNs: UInt64) {
        guard isMeasuring else { return }
        var fp: UInt64?
        if #available(macOS 14.4, *) {
            if let pb = renderer.displayedPixelBuffer() {
                fp = LumaFingerprint.of(pb)
                let f = Self.fourCC(CVPixelBufferGetPixelFormatType(pb))
                let desc = "copy \(f) \(CVPixelBufferGetWidth(pb))×\(CVPixelBufferGetHeight(pb))"
                lock.lock()
                copyFormat = desc
                lock.unlock()
            }
        }
        matcher.observed(fingerprint: fp, atNs: atNs)
        publishIfDue(nowNs: atNs)
    }

    private func publishIfDue(nowNs: UInt64) {
        lock.lock()
        if windowStartNs == 0 { windowStartNs = nowNs }
        let seconds = Double(nowNs - min(nowNs, windowStartNs)) / 1e9
        guard seconds >= 1 else { return lock.unlock() }
        windowStartNs = nowNs
        let extra = [metricsText, copyFormat.isEmpty ? nil : copyFormat].compactMap { $0 }
        lock.unlock()
        if let s = matcher.takeWindow().summary(seconds: seconds, extra: extra) { onStats(s) }
        loadMetrics()
    }

    // MARK: the layer's own performance metrics (iOS 17.4+)

    struct Metrics {
        var frames = 0, dropped = 0, corrupted = 0, optimized = 0
        var delay: TimeInterval = 0
    }

    private func loadMetrics() {
        guard #available(macOS 14.4, *) else { return }
        renderer.loadVideoPerformanceMetrics { [weak self] m in
            guard let self, let m else { return }
            let now = Metrics(frames: m.totalNumberOfFrames, dropped: m.numberOfDroppedFrames,
                              corrupted: m.numberOfCorruptedFrames,
                              optimized: m.numberOfFramesDisplayedUsingOptimizedCompositing,
                              delay: m.totalAccumulatedFrameDelay)
            self.lock.lock()
            if let prev = self.lastMetrics {
                self.metricsText = Self.describe(now, since: prev)
            }
            self.lastMetrics = now
            self.lock.unlock()
        }
    }

    /// The change between two snapshots (counters restart after a flush).
    static func describe(_ now: Metrics, since prev: Metrics) -> String {
        let restarted = now.frames < prev.frames
        let base = restarted ? Metrics() : prev
        let frames = now.frames - base.frames
        guard frames > 0 else { return "layer: no frames" }
        let optimized = (now.optimized - base.optimized) * 100 / frames
        let delayMs = (now.delay - base.delay) * 1000 / Double(frames)
        return "layer: \(frames) frames, \(now.dropped - base.dropped) dropped, optimized compositing \(optimized)%, "
            + String(format: "frame delay %.1f ms", delayMs)
    }

    private static func notQueued(_ frame: VideoFrame, _ receivedNs: UInt64) -> VideoFeedback {
        VideoFeedback(frameID: frame.frameID, requestKeyframe: true, receivedNs: receivedNs, decodedNs: 0,
                      displayedNs: 0)
    }

    static func fourCC(_ t: OSType) -> String {
        let bytes = [UInt8(t >> 24 & 0xFF), UInt8(t >> 16 & 0xFF), UInt8(t >> 8 & 0xFF), UInt8(t & 0xFF)]
        let printable = bytes.allSatisfy { $0 >= 0x20 && $0 < 0x7F }
        return printable ? String(decoding: bytes, as: UTF8.self) : String(t)
    }
}

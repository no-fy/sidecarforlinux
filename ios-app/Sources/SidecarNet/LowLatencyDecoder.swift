// Phase 2, Metal renderer: decode with VideoToolbox ourselves, so the app
// knows when each frame was decoded and when it reached the screen (the
// AVSampleBufferDisplayLayer path can't tell).
//
// No UIKit here: the decoder, the newest-frame slot and the frame reports
// are tested on macOS in CI (Tests/SidecarNetTests/MetalVideoTests.swift).
import CoreMedia
import CoreVideo
import Foundation
import SidecarProtocol
import VideoToolbox

/// A decoded picture on its way to the screen. `finish` sends the PC its
/// VideoFeedback, exactly once, however the frame ends (shown, replaced by a
/// newer one, or dropped).
public final class DecodedVideoFrame: @unchecked Sendable {
    public let frameID: UInt32
    public let receivedNs: UInt64
    public let decodedNs: UInt64
    /// NV12, IOSurface-backed and Metal-compatible.
    public let image: CVPixelBuffer
    private let report: (VideoFeedback) -> Void
    private let lock = NSLock()
    private var done = false

    init(frameID: UInt32, receivedNs: UInt64, decodedNs: UInt64, image: CVPixelBuffer,
         report: @escaping (VideoFeedback) -> Void) {
        self.frameID = frameID
        self.receivedNs = receivedNs
        self.decodedNs = decodedNs
        self.image = image
        self.report = report
    }

    /// `displayedNs`: session clock when the frame reached the screen, 0 if
    /// it never did. Later calls do nothing.
    public func finish(displayedNs: UInt64) {
        lock.lock()
        let first = !done
        done = true
        lock.unlock()
        guard first else { return }
        report(VideoFeedback(frameID: frameID, requestKeyframe: false, receivedNs: receivedNs,
                             decodedNs: decodedNs, displayedNs: displayedNs))
    }

    deinit { finish(displayedNs: 0) }
}

/// Holds only the newest value. A `put` replaces the value nobody took yet
/// and returns it, so stale frames are dropped instead of queued.
public final class LatestSlot<T>: @unchecked Sendable {
    private let cond = NSCondition()
    private var value: T?
    private var closed = false

    public init() {}

    @discardableResult
    public func put(_ v: T) -> T? {
        cond.lock()
        defer { cond.unlock() }
        let old = value
        value = v
        cond.signal()
        return old
    }

    /// Waits for a value; nil at the deadline or once closed.
    public func take(waitingUntil deadline: Date = .distantFuture) -> T? {
        cond.lock()
        defer { cond.unlock() }
        while value == nil && !closed {
            if !cond.wait(until: deadline) { return nil }
        }
        let v = value
        value = nil
        return v
    }

    /// The current value, without waiting.
    public func poll() -> T? {
        cond.lock()
        defer { cond.unlock() }
        let v = value
        value = nil
        return v
    }

    public func close() {
        cond.lock()
        closed = true
        cond.broadcast()
        cond.unlock()
    }
}

/// Asynchronous VTDecompressionSession tuned for latency:
/// * real-time mode, no power-saving hints;
/// * no temporal processing, so frames come out in decode order as soon as
///   each one is done (our streams have no B-frames, nothing to reorder);
/// * asynchronous, so a 2–5 ms decode never blocks the network queue that
///   also carries the pen;
/// * output buffers are NV12 in IOSurfaces, which Metal reads without a copy.
///
/// Call `configure`/`decode`/`invalidate` from one queue (PeerServer.queue).
/// `onFrame` runs on a VideoToolbox thread.
public final class LowLatencyDecoder: @unchecked Sendable {
    public private(set) var format: VideoFormat?
    private var session: VTDecompressionSession?
    private let onFrame: (DecodedVideoFrame) -> Void
    // Shared with VideoToolbox's callback threads.
    private let lock = NSLock()
    private var needKeyframe = true
    private var sessionBroken = false
    private var generation = 0

    public init(onFrame: @escaping (DecodedVideoFrame) -> Void) {
        self.onFrame = onFrame
    }

    /// New parameter sets. The same stream keeps its session; a different
    /// one gets a new session that waits for a keyframe.
    public func configure(_ config: VideoConfig) throws {
        let f = try VideoFormat(config)
        if let old = format, session != nil, !isBroken,
           CMFormatDescriptionEqual(old.formatDescription, otherFormatDescription: f.formatDescription) {
            format = f
            return
        }
        format = f
        try makeSession()
    }

    /// Decodes `frame`. Frames that can't be decoded are reported at once
    /// with `requestKeyframe`; decoded ones go to `onFrame`, which must
    /// eventually `finish` them.
    public func decode(_ frame: VideoFrame, receivedNs: UInt64, report: @escaping (VideoFeedback) -> Void) {
        let failed = {
            report(VideoFeedback(frameID: frame.frameID, requestKeyframe: true, receivedNs: receivedNs,
                                 decodedNs: 0, displayedNs: 0))
        }
        if isBroken && format != nil {
            // e.g. iOS invalidated the session while the app was in the background.
            try? makeSession()
        }
        guard let session, let format else { return failed() }
        if waitingForKeyframe {
            guard frame.keyframe else { return failed() }
            lock.lock()
            needKeyframe = false
            lock.unlock()
        }
        guard let sample = try? format.sampleBuffer(frame, displayImmediately: false) else { return failed() }

        let once = Once()
        let gen = currentGeneration
        let onFrame = self.onFrame
        let id = frame.frameID
        let status = VTDecompressionSessionDecodeFrame(
            session, sampleBuffer: sample, flags: [._EnableAsynchronousDecompression], infoFlagsOut: nil
        ) { [weak self] st, info, image, _, _ in
            let now = SessionClock.nowNs()
            guard st == noErr, let image, !info.contains(.frameDropped) else {
                if st == kVTInvalidSessionErr { self?.markBroken() }
                once.run(failed)
                return
            }
            guard let self, self.currentGeneration == gen else {
                once.run(failed)  // decoded for a session that was replaced meanwhile
                return
            }
            once.run {
                onFrame(DecodedVideoFrame(frameID: id, receivedNs: receivedNs, decodedNs: now, image: image,
                                          report: report))
            }
        }
        if status != noErr {
            if status == kVTInvalidSessionErr { markBroken() }
            once.run(failed)
        }
    }

    /// Ends the session (video stopped). Frames still decoding are reported
    /// as not decoded.
    public func invalidate() {
        invalidateSession()
        format = nil
    }

    /// Blocks until every frame handed to `decode` has come out (tests).
    public func waitForPendingFrames() {
        if let session { VTDecompressionSessionWaitForAsynchronousFrames(session) }
    }

    public var waitingForKeyframe: Bool {
        lock.lock()
        defer { lock.unlock() }
        return needKeyframe
    }

    // MARK: internals

    private var isBroken: Bool {
        lock.lock()
        defer { lock.unlock() }
        return sessionBroken
    }

    private var currentGeneration: Int {
        lock.lock()
        defer { lock.unlock() }
        return generation
    }

    private func markBroken() {
        lock.lock()
        sessionBroken = true
        lock.unlock()
    }

    private func makeSession() throws {
        invalidateSession()
        guard let format else { throw VideoError.notConfigured }
        var s: VTDecompressionSession?
        let status = VTDecompressionSessionCreate(
            allocator: kCFAllocatorDefault, formatDescription: format.formatDescription,
            decoderSpecification: nil, imageBufferAttributes: Self.outputAttributes(fullRange: format.isFullRange),
            outputCallback: nil, decompressionSessionOut: &s)
        guard status == noErr, let s else { throw VideoError.session(status) }
        // Best effort: a decoder that doesn't know a property just ignores it.
        _ = VTSessionSetProperty(s, key: kVTDecompressionPropertyKey_RealTime, value: kCFBooleanTrue)
        _ = VTSessionSetProperty(s, key: kVTDecompressionPropertyKey_MaximizePowerEfficiency, value: kCFBooleanFalse)
        session = s
        lock.lock()
        needKeyframe = true
        sessionBroken = false
        lock.unlock()
    }

    private func invalidateSession() {
        lock.lock()
        generation += 1
        lock.unlock()
        if let session {
            VTDecompressionSessionWaitForAsynchronousFrames(session)
            VTDecompressionSessionInvalidate(session)
        }
        session = nil
    }

    static func outputAttributes(fullRange: Bool) -> CFDictionary {
        let attrs: [CFString: Any] = [
            kCVPixelBufferPixelFormatTypeKey: fullRange
                ? kCVPixelFormatType_420YpCbCr8BiPlanarFullRange
                : kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
            kCVPixelBufferIOSurfacePropertiesKey: [String: Any](),
            kCVPixelBufferMetalCompatibilityKey: true,
        ]
        return attrs as CFDictionary
    }

    deinit {
        if let session { VTDecompressionSessionInvalidate(session) }
    }
}

/// Runs its block at most once, from whichever thread gets there first.
final class Once: @unchecked Sendable {
    private let lock = NSLock()
    private var done = false

    func run(_ f: () -> Void) {
        lock.lock()
        let first = !done
        done = true
        lock.unlock()
        if first { f() }
    }
}

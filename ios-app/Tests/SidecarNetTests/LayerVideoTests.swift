// The AVSampleBufferDisplayLayer renderer's display timing, on macOS:
// picture fingerprints, matching what the layer shows to the frames that
// were enqueued, the readout, and the sink on a real (offscreen)
// AVSampleBufferDisplayLayer. Whether the layer hands out its picture
// offscreen is up to macOS; on the iPad it's on screen.
import AVFoundation
import CoreVideo
import Foundation
import SidecarProtocol
import VideoToolbox
import XCTest

@testable import SidecarNet

final class LayerVideoTests: XCTestCase {
    let ms: UInt64 = 1_000_000

    func vector() throws -> (VideoConfig, [VideoFrame]) {
        var config: VideoConfig?
        var frames: [VideoFrame] = []
        for m in try VideoDecoderTests.messages() {
            switch m {
            case .videoConfig(let c): config = c
            case .videoFrame(let f): frames.append(f)
            default: break
            }
        }
        return (try XCTUnwrap(config), frames)
    }

    /// An NV12 picture with luma `y(x, row)` and flat chroma `c`, rows
    /// aligned to `alignment` bytes (so the same picture can have different
    /// row padding).
    func nv12(width: Int = 32, height: Int = 16, alignment: Int = 16, chroma: UInt8 = 128,
              _ y: (Int, Int) -> UInt8) throws -> CVPixelBuffer {
        let attrs: [CFString: Any] = [
            kCVPixelBufferIOSurfacePropertiesKey: [String: Any](),
            kCVPixelBufferBytesPerRowAlignmentKey: alignment,
        ]
        var pb: CVPixelBuffer?
        XCTAssertEqual(CVPixelBufferCreate(kCFAllocatorDefault, width, height,
                                           kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange, attrs as CFDictionary, &pb),
                       kCVReturnSuccess)
        let image = try XCTUnwrap(pb)
        CVPixelBufferLockBaseAddress(image, [])
        let yp = try XCTUnwrap(CVPixelBufferGetBaseAddressOfPlane(image, 0)).assumingMemoryBound(to: UInt8.self)
        let cp = try XCTUnwrap(CVPixelBufferGetBaseAddressOfPlane(image, 1)).assumingMemoryBound(to: UInt8.self)
        let ys = CVPixelBufferGetBytesPerRowOfPlane(image, 0), cs = CVPixelBufferGetBytesPerRowOfPlane(image, 1)
        for r in 0..<height { for c in 0..<ys { yp[r * ys + c] = c < width ? y(c, r) : UInt8(truncatingIfNeeded: c &* 7) } }
        for r in 0..<height / 2 { for c in 0..<cs { cp[r * cs + c] = chroma } }
        CVPixelBufferUnlockBaseAddress(image, [])
        return image
    }

    // MARK: fingerprints

    func testFingerprintIsAboutLumaOnly() throws {
        let a = try nv12 { x, y in UInt8((x * 3 + y * 5) & 0xFF) }
        let padded = try nv12(alignment: 128) { x, y in UInt8((x * 3 + y * 5) & 0xFF) }
        XCTAssertNotEqual(CVPixelBufferGetBytesPerRowOfPlane(a, 0), CVPixelBufferGetBytesPerRowOfPlane(padded, 0),
                          "the test needs two different row paddings")
        let otherChroma = try nv12(chroma: 90) { x, y in UInt8((x * 3 + y * 5) & 0xFF) }
        let onePixel = try nv12 { x, y in x == 17 && y == 9 ? 0 : UInt8((x * 3 + y * 5) & 0xFF) }
        let wider = try nv12(width: 34) { x, y in UInt8((x * 3 + y * 5) & 0xFF) }
        let fp = try XCTUnwrap(LumaFingerprint.of(a))
        XCTAssertEqual(LumaFingerprint.of(padded), fp, "row padding is ignored")
        XCTAssertEqual(LumaFingerprint.of(otherChroma), fp, "chroma is ignored")
        XCTAssertNotEqual(LumaFingerprint.of(onePixel), fp, "one changed luma pixel changes it")
        XCTAssertNotEqual(LumaFingerprint.of(wider), fp, "the size is part of it")
    }

    /// Only plain layouts are read with the CPU. The iPad's layer hands out
    /// "&8v0" (lossless compressed NV12; the crash on 2026-10-31 had it in a
    /// register), whose bytes per row describe tiles, not rows.
    func testFingerprintReadsOnlyPlainLayouts() throws {
        var pb: CVPixelBuffer?
        XCTAssertEqual(CVPixelBufferCreate(kCFAllocatorDefault, 32, 16, kCVPixelFormatType_422YpCbCr8, nil, &pb),
                       kCVReturnSuccess)
        XCTAssertNil(LumaFingerprint.of(try XCTUnwrap(pb)), "a layout we don't know is not read")
        XCTAssertNil(LumaFingerprint.lumaBytesPerPixel(Self.lossless420v))
    }

    static let lossless420v: OSType = 0x2638_7630  // '&8v0'

    /// The layer's copy goes through PlainPicture: a plain NV12 copy with
    /// the same luma, in a buffer reused for the next picture.
    func testPlainPictureCopiesExactly() throws {
        let a = try nv12(alignment: 128) { x, y in UInt8((x * 3 + y * 5) & 0xFF) }
        let b = try nv12(alignment: 128) { x, y in UInt8((x * 11 + y) & 0xFF) }
        let plain = PlainPicture()
        let fmt = kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange
        XCTAssertEqual(LumaFingerprint.of(try XCTUnwrap(plain.copy(a, as: fmt))), LumaFingerprint.of(a))
        XCTAssertEqual(LumaFingerprint.of(try XCTUnwrap(plain.copy(b, as: fmt))), LumaFingerprint.of(b))
        XCTAssertEqual(LumaFingerprint.of(try XCTUnwrap(plain.copy(a, as: fmt))), LumaFingerprint.of(a))
    }

    /// What the iPad does: the picture arrives as "&8v0". Skipped where this
    /// Mac can't make compressed buffers.
    func testCompressedPictureRoundTrip() throws {
        let a = try nv12(width: 256, height: 128) { x, y in UInt8((x * 3 + y * 5) & 0xFF) }
        let attrs: [CFString: Any] = [kCVPixelBufferIOSurfacePropertiesKey: [String: Any]()]
        var pb: CVPixelBuffer?
        guard CVPixelBufferCreate(kCFAllocatorDefault, 256, 128, Self.lossless420v, attrs as CFDictionary, &pb)
            == kCVReturnSuccess, let compressed = pb
        else { throw XCTSkip("no &8v0 buffers on this Mac") }
        var s: VTPixelTransferSession?
        XCTAssertEqual(VTPixelTransferSessionCreate(allocator: kCFAllocatorDefault, pixelTransferSessionOut: &s), noErr)
        let session = try XCTUnwrap(s)
        defer { VTPixelTransferSessionInvalidate(session) }
        guard VTPixelTransferSessionTransferImage(session, from: a, to: compressed) == noErr
        else { throw XCTSkip("this Mac can't write &8v0") }
        XCTAssertNil(LumaFingerprint.of(compressed), "never read with the CPU")
        let back = try XCTUnwrap(PlainPicture().copy(compressed, as: kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange))
        XCTAssertEqual(LumaFingerprint.of(back), LumaFingerprint.of(a), "lossless: the luma comes back unchanged")
        print("&8v0 round trip: luma unchanged")
    }

    /// Our decode and a byte-for-byte copy with other row padding (what the
    /// layer's copy is, if bit-exact) fingerprint the same; different
    /// frames of the clip don't.
    func testDecodedFramesAndTheirCopiesMatch() throws {
        let (config, frames) = try vector()
        let decoded = Locked<[DecodedVideoFrame]>([])
        let decoder = LowLatencyDecoder { f in decoded.mutate { $0.append(f) } }
        try decoder.configure(config)
        for f in frames { decoder.decode(f, receivedNs: 1) { _ in } }
        decoder.waitForPendingFrames()
        let images = decoded.value.map(\.image)
        XCTAssertEqual(images.count, frames.count)
        let fps = images.compactMap { LumaFingerprint.of($0) }
        XCTAssertEqual(fps.count, images.count)
        print("clip fingerprints: \(Set(fps).count) distinct of \(fps.count)")
        XCTAssertGreaterThan(Set(fps).count, 1, "a moving clip has different pictures")

        let src = images[0]
        CVPixelBufferLockBaseAddress(src, .readOnly)
        defer { CVPixelBufferUnlockBaseAddress(src, .readOnly) }
        let sp = try XCTUnwrap(CVPixelBufferGetBaseAddressOfPlane(src, 0)).assumingMemoryBound(to: UInt8.self)
        let ss = CVPixelBufferGetBytesPerRowOfPlane(src, 0)
        let copy = try nv12(width: CVPixelBufferGetWidth(src), height: CVPixelBufferGetHeight(src), alignment: 512) {
            x, y in sp[y * ss + x]
        }
        XCTAssertEqual(LumaFingerprint.of(copy), fps[0])
    }

    // MARK: matching

    final class Reports: @unchecked Sendable {
        let items = Locked<[VideoFeedback]>([])
        func add(_ fb: VideoFeedback) { items.mutate { $0.append(fb) } }
        func displayed(_ id: UInt32) -> [UInt64] { items.value.filter { $0.frameID == id }.map(\.displayedNs) }
    }

    func testMatchesTheOldestFrameShowingThePicture() {
        let m = DisplayMatcher()
        let r = Reports()
        for (id, rx) in [(1, 10), (2, 20), (3, 30), (4, 40)] as [(UInt32, UInt64)] {
            m.enqueued(frameID: id, receivedNs: rx * ms, report: r.add)
        }
        // 2 and 3 look the same (nothing moved between them).
        for (id, fp) in [(1, 0xA), (2, 0xB), (3, 0xB), (4, 0xC)] as [(UInt32, UInt64)] {
            m.fingerprinted(frameID: id, fingerprint: fp)
        }
        XCTAssertTrue(r.items.value.isEmpty, "nothing is known before the screen is looked at")
        m.observed(fingerprint: 0xA, atNs: 50 * ms)
        m.observed(fingerprint: 0xA, atNs: 66 * ms)  // unchanged
        m.observed(fingerprint: 0xB, atNs: 83 * ms)
        XCTAssertEqual(r.displayed(2), [83 * ms], "the oldest frame with that picture")
        XCTAssertEqual(r.displayed(3), [], "its twin is still open")
        m.observed(fingerprint: 0xC, atNs: 100 * ms)
        XCTAssertEqual(r.items.value.map(\.frameID), [1, 2, 3, 4], "every frame reported once, in order")
        XCTAssertEqual(r.items.value.map(\.displayedNs), [50 * ms, 83 * ms, 0, 100 * ms])
        XCTAssertTrue(r.items.value.allSatisfy { $0.decodedNs == 0 && !$0.requestKeyframe },
                      "decoding happens inside the layer: unknowable")
        let w = m.takeWindow()
        XCTAssertEqual(w.polls, 4)
        XCTAssertEqual(w.changes, 3)
        XCTAssertEqual(w.exactUs, [40_000, 63_000, 60_000])
        XCTAssertEqual(w.unmatched, 0)
    }

    func testWaitsForTheAppsOwnDecode() {
        let m = DisplayMatcher()
        let r = Reports()
        m.enqueued(frameID: 1, receivedNs: 10 * ms, report: r.add)
        m.observed(fingerprint: 0xA, atNs: 30 * ms)  // shown before our decode finished
        XCTAssertTrue(r.items.value.isEmpty)
        m.fingerprinted(frameID: 1, fingerprint: 0xA)
        XCTAssertEqual(r.displayed(1), [30 * ms], "timed at the refresh where it appeared")
    }

    func testFallsBackToTheFirstFrameAfterAPause() {
        let m = DisplayMatcher()
        let r = Reports()
        // The layer's copy doesn't match any decode (e.g. not bit-identical).
        m.enqueued(frameID: 1, receivedNs: 1000 * ms, report: r.add)  // after a pause (first frame)
        m.enqueued(frameID: 2, receivedNs: 1010 * ms, report: r.add)  // 10 ms later: no pause
        m.fingerprinted(frameID: 1, fingerprint: 0x1)
        m.fingerprinted(frameID: 2, fingerprint: 0x2)
        m.observed(fingerprint: 0x99, atNs: 1040 * ms)
        XCTAssertEqual(r.displayed(1), [1040 * ms], "the change belongs to the frame that ended the pause")
        m.observed(fingerprint: 0x98, atNs: 1056 * ms)  // can't be attributed: no pause
        XCTAssertEqual(r.displayed(2), [])
        m.observed(fingerprint: nil, atNs: 1600 * ms)  // much later: frame 2 expires
        XCTAssertEqual(r.displayed(2), [0])
        let w = m.takeWindow()
        XCTAssertEqual(w.afterPauseUs, [40_000])
        XCTAssertEqual(w.exactUs, [])
        XCTAssertEqual(w.unmatched, 2)
        XCTAssertEqual(w.nilPolls, 1)

        // After a 200 ms gap, the next frame is again "after a pause".
        m.enqueued(frameID: 3, receivedNs: 1800 * ms, report: r.add)
        m.fingerprinted(frameID: 3, fingerprint: 0x3)
        m.observed(fingerprint: 0x97, atNs: 1830 * ms)
        XCTAssertEqual(r.displayed(3), [1830 * ms])
    }

    func testFlushReportsEveryOpenFrameOnce() {
        let m = DisplayMatcher()
        let r = Reports()
        m.enqueued(frameID: 1, receivedNs: 10 * ms, report: r.add)
        m.enqueued(frameID: 2, receivedNs: 20 * ms, report: r.add)
        m.flush()
        m.flush()
        XCTAssertEqual(r.items.value.map(\.frameID), [1, 2])
        XCTAssertEqual(r.items.value.map(\.displayedNs), [0, 0])
    }

    // MARK: readout

    func testReadout() throws {
        var w = LayerWindow()
        XCTAssertNil(w.summary(seconds: 1))
        w.polls = 60
        w.nilPolls = 60
        XCTAssertEqual(w.summary(seconds: 1),
                       "AVSampleBufferDisplayLayer: not measurable, displayedPixelBuffer() gave nothing in 60 refreshes")
        var u = LayerWindow()
        u.polls = 60
        u.nilPolls = 60
        u.unreadablePolls = 60
        XCTAssertEqual(u.summary(seconds: 1),
                       "AVSampleBufferDisplayLayer: not measurable, the layer's picture couldn't be read in 60 refreshes")
        w.nilPolls = 2
        w.changes = 30
        for v in [40, 42, 50] { w.add(receivedToScreenNs: UInt64(v) * ms, match: .exact) }
        w.add(receivedToScreenNs: 38 * ms, match: .afterPause)
        w.pollCostUs = [300, 500, 2100]
        let s = try XCTUnwrap(w.summary(seconds: 1, extra: ["layer: 60 frames, 0 dropped", "copy 420v 2360×1640"]))
        XCTAssertEqual(s, "AVSampleBufferDisplayLayer: received → screen 42.0/50.0 ms (±1 refresh) · 3 matched/s · "
                          + "after a pause 38.0 ms (1) · 2/60 refreshes without a picture · poll 0.5/2.1 ms · "
                          + "layer: 60 frames, 0 dropped · copy 420v 2360×1640")
    }

    func testLayerMetricsDeltas() {
        let a = LayerVideoSink.Metrics(frames: 100, dropped: 2, corrupted: 0, optimized: 50, delay: 1.0)
        let b = LayerVideoSink.Metrics(frames: 160, dropped: 3, corrupted: 0, optimized: 110, delay: 1.3)
        XCTAssertEqual(LayerVideoSink.describe(b, since: a),
                       "layer: 60 frames, 1 dropped, optimized compositing 100%, frame delay 5.0 ms")
        // Counters restart after a flush: the new values are the delta.
        let c = LayerVideoSink.Metrics(frames: 30, dropped: 0, corrupted: 0, optimized: 0, delay: 0.03)
        XCTAssertEqual(LayerVideoSink.describe(c, since: b),
                       "layer: 30 frames, 0 dropped, optimized compositing 0%, frame delay 1.0 ms")
        XCTAssertEqual(LayerVideoSink.fourCC(kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange), "420v")
    }

    // MARK: the sink on a real AVSampleBufferDisplayLayer

    /// Every frame is reported exactly once whatever the layer does
    /// offscreen; prints what `displayedPixelBuffer()` and the performance
    /// metrics gave on this Mac.
    func testLayerSinkOnARealLayer() throws {
        let (config, frames) = try vector()
        let layer = AVSampleBufferDisplayLayer()
        layer.frame = CGRect(x: 0, y: 0, width: 176, height: 144)
        let readouts = Locked<[String]>([])
        let sink = LayerVideoSink(renderer: layer.sampleBufferRenderer) { s in readouts.mutate { $0.append(s) } }
        let log = FeedbackLog()
        sink.configure(config)
        sink.setMeasuring(true)
        var pollMs: [Double] = []
        func refresh(_ n: Int) {
            for _ in 0..<n {
                RunLoop.current.run(until: Date().addingTimeInterval(0.016))
                let t0 = SessionClock.nowNs()
                sink.pollDisplayed(atNs: t0)
                pollMs.append(Double(SessionClock.nowNs() - t0) / 1e6)
            }
        }
        for f in frames {
            sink.enqueue(f, receivedNs: SessionClock.nowNs(), report: log.add)
            refresh(2)
        }
        refresh(140)  // ~2.3 s: two readouts, the second with the layer's metrics
        sink.setMeasuring(false)  // reports whatever is still open
        let fb = log.all
        let shown = fb.filter { $0.displayedNs != 0 }
        let sorted = pollMs.sorted()
        print("AVSampleBufferDisplayLayer on macOS: \(fb.count)/\(frames.count) reported, \(shown.count) with a display time; "
              + String(format: "pollDisplayed p50 %.2f ms, max %.2f ms over %d polls",
                       sorted[sorted.count / 2], sorted.last ?? 0, sorted.count))
        for r in readouts.value { print("  readout: \(r)") }
        XCTAssertEqual(fb.count, frames.count, "every frame reported")
        XCTAssertEqual(Set(fb.map(\.frameID)).count, fb.count, "and only once")
        for x in fb {
            XCTAssertEqual(x.decodedNs, 0)
            if x.displayedNs != 0 { XCTAssertGreaterThanOrEqual(x.displayedNs, x.receivedNs) }
        }
        XCTAssertFalse(readouts.value.isEmpty, "a readout comes once a second while measuring")
    }
}

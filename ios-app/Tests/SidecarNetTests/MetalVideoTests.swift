// The Metal video renderer's building blocks, on macOS (same VideoToolbox,
// CoreVideo and Metal as the iPad): asynchronous decode into Metal-ready
// NV12, per-frame reports, newest-frame-wins, the YCbCr → RGB shader and
// aspect-fit drawing. Only the on-screen CAMetalLayer presentation needs an
// iPad. GPU tests are skipped when the runner has no Metal device.
import CoreMedia
import CoreVideo
import Foundation
import Metal
import QuartzCore
import SidecarProtocol
import XCTest

@testable import SidecarNet

final class Locked<T>: @unchecked Sendable {
    private let lock = NSLock()
    private var v: T
    init(_ v: T) { self.v = v }
    var value: T {
        lock.lock()
        defer { lock.unlock() }
        return v
    }
    func mutate(_ f: (inout T) -> Void) {
        lock.lock()
        f(&v)
        lock.unlock()
    }
}

final class FeedbackLog: @unchecked Sendable {
    let items = Locked<[VideoFeedback]>([])
    func add(_ fb: VideoFeedback) { items.mutate { $0.append(fb) } }
    var all: [VideoFeedback] { items.value }
}

/// Stands in for the Metal renderer without a GPU: decodes with
/// LowLatencyDecoder and "shows" each frame 1 ms later.
final class DecodeAndShowSink: VideoFrameSink, @unchecked Sendable {
    private var decoder: LowLatencyDecoder!
    let frames = Locked<[VideoFrame]>([])
    let configs = Locked<[VideoConfig]>([])

    init() {
        decoder = LowLatencyDecoder { f in
            DispatchQueue.global().asyncAfter(deadline: .now() + .milliseconds(1)) {
                f.finish(displayedNs: SessionClock.nowNs())
            }
        }
    }

    func configure(_ config: VideoConfig) {
        configs.mutate { $0.append(config) }
        try? decoder.configure(config)
    }

    func enqueue(_ frame: VideoFrame, receivedNs: UInt64, report: @escaping (VideoFeedback) -> Void) {
        frames.mutate { $0.append(frame) }
        decoder.decode(frame, receivedNs: receivedNs, report: report)
    }

    func reset() { decoder.invalidate() }
}

final class RecordingSink: VideoFrameSink, @unchecked Sendable {
    let events = Locked<[String]>([])
    func configure(_ config: VideoConfig) { events.mutate { $0.append("config") } }
    func enqueue(_ frame: VideoFrame, receivedNs: UInt64, report: @escaping (VideoFeedback) -> Void) {
        events.mutate { $0.append("frame \(frame.frameID)") }
        report(VideoFeedback(frameID: frame.frameID, requestKeyframe: false, receivedNs: receivedNs, decodedNs: 0,
                             displayedNs: 0))
    }
    func reset() { events.mutate { $0.append("reset") } }
}

final class MetalVideoTests: XCTestCase {
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

    // MARK: decoder

    func testDecoderOutputsMetalReadyNV12AndReportsEachFrameOnce() throws {
        let (config, frames) = try vector()
        let log = FeedbackLog()
        let decoded = Locked<[DecodedVideoFrame]>([])
        let decoder = LowLatencyDecoder { f in decoded.mutate { $0.append(f) } }
        try decoder.configure(config)
        XCTAssertTrue(decoder.waitingForKeyframe)
        let fullRange = try XCTUnwrap(decoder.format).isFullRange
        for f in frames {
            decoder.decode(f, receivedNs: SessionClock.nowNs(), report: log.add)
        }
        decoder.waitForPendingFrames()

        let out = decoded.value
        XCTAssertEqual(out.map(\.frameID), frames.map(\.frameID), "every frame decoded, in decode order")
        XCTAssertTrue(log.all.isEmpty, "decoded frames are reported once their fate is known, not before")
        for f in out {
            XCTAssertNotNil(CVPixelBufferGetIOSurface(f.image), "IOSurface-backed: Metal reads it without a copy")
            XCTAssertEqual(CVPixelBufferGetPixelFormatType(f.image),
                           fullRange ? kCVPixelFormatType_420YpCbCr8BiPlanarFullRange
                                     : kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange)
            XCTAssertEqual(CVPixelBufferGetWidth(f.image), 176)
            XCTAssertEqual(CVPixelBufferGetHeight(f.image), 144)
            XCTAssertGreaterThanOrEqual(f.decodedNs, f.receivedNs)
        }
        let decodeMs = out.map { Double($0.decodedNs - $0.receivedNs) / 1e6 }
        print("decode times (ms): \(decodeMs.map { String(format: "%.2f", $0) })")

        for f in out { f.finish(displayedNs: f.decodedNs + 1000) }
        for f in out { f.finish(displayedNs: 1) }  // later calls change nothing
        let fb = log.all
        XCTAssertEqual(fb.map(\.frameID), frames.map(\.frameID))
        for x in fb {
            XCTAssertFalse(x.requestKeyframe)
            XCTAssertGreaterThan(x.decodedNs, 0)
            XCTAssertEqual(x.displayedNs, x.decodedNs + 1000)
        }
    }

    func testWaitsForAKeyframeAndAsksForOne() throws {
        let (config, frames) = try vector()
        let key = try XCTUnwrap(frames.first { $0.keyframe })
        let delta = try XCTUnwrap(frames.first { !$0.keyframe })
        let log = FeedbackLog()
        let decoded = Locked<[UInt32]>([])
        let decoder = LowLatencyDecoder { f in
            decoded.mutate { $0.append(f.frameID) }
            f.finish(displayedNs: 0)
        }

        decoder.decode(key, receivedNs: 5, report: log.add)  // not configured yet
        try decoder.configure(config)
        decoder.decode(delta, receivedNs: 6, report: log.add)  // a P-frame before any keyframe
        decoder.decode(key, receivedNs: 7, report: log.add)
        decoder.waitForPendingFrames()

        XCTAssertEqual(decoded.value, [key.frameID])
        let fb = log.all
        XCTAssertEqual(fb.count, 3)
        XCTAssertEqual(fb.prefix(2).map(\.requestKeyframe), [true, true])
        XCTAssertEqual(fb.prefix(2).map(\.decodedNs), [0, 0])
        XCTAssertEqual(fb.prefix(2).map(\.receivedNs), [5, 6])
        XCTAssertFalse(decoder.waitingForKeyframe)

        // The same parameter sets again (the daemon's file source repeats
        // them; the renderer switch replays them): the session carries on.
        try decoder.configure(config)
        XCTAssertFalse(decoder.waitingForKeyframe)

        decoder.invalidate()
        decoder.decode(delta, receivedNs: 8, report: log.add)
        XCTAssertEqual(log.all.last?.requestKeyframe, true)
    }

    // MARK: newest frame wins

    func testLatestSlotKeepsOnlyTheNewest() {
        let slot = LatestSlot<Int>()
        XCTAssertNil(slot.put(1))
        XCTAssertEqual(slot.put(2), 1, "the replaced value comes back, to be reported as not shown")
        XCTAssertEqual(slot.take(), 2)
        XCTAssertNil(slot.poll())
        XCTAssertNil(slot.take(waitingUntil: Date().addingTimeInterval(0.05)))

        // A slow consumer sees increasing values and always gets the last one.
        let seen = Locked<[Int]>([])
        let done = expectation(description: "consumer")
        let consumer = Thread {
            while let v = slot.take(waitingUntil: Date().addingTimeInterval(2)) {
                seen.mutate { $0.append(v) }
                if v == 500 { break }
                usleep(300)
            }
            done.fulfill()
        }
        consumer.start()
        for i in 1...500 { slot.put(i) }
        wait(for: [done], timeout: 5)
        let s = seen.value
        XCTAssertEqual(s.last, 500)
        XCTAssertEqual(s, s.sorted())
        XCTAssertEqual(Set(s).count, s.count)
        print("slow consumer saw \(s.count) of 500 values")

        slot.close()
        XCTAssertNil(slot.take())
    }

    func testRendererSwitchReplaysParameterSetsToTheNewRenderer() throws {
        let (config, frames) = try vector()
        let a = RecordingSink()
        let b = RecordingSink()
        let sw = VideoSinkSwitch([a, b], selected: 0)
        let log = FeedbackLog()
        sw.configure(config)
        sw.enqueue(frames[0], receivedNs: 1, report: log.add)
        sw.select(1)
        XCTAssertEqual(sw.selected, 1)
        sw.enqueue(frames[1], receivedNs: 2, report: log.add)
        XCTAssertEqual(a.events.value, ["config", "frame 1", "reset"])
        XCTAssertEqual(b.events.value, ["config", "frame 2"])
        XCTAssertEqual(log.all.map(\.frameID), [1, 2])

        // Video ended: no parameter sets to replay on the next switch.
        sw.reset()
        sw.select(0)
        sw.enqueue(frames[2], receivedNs: 3, report: log.add)
        XCTAssertEqual(b.events.value, ["config", "frame 2", "reset", "reset"])
        XCTAssertEqual(a.events.value, ["config", "frame 1", "reset", "frame 3"])
    }

    // MARK: geometry, colour, clock

    func testAspectFit() {
        XCTAssertEqual(aspectFitRect(contentWidth: 2360, contentHeight: 1640, targetWidth: 2360, targetHeight: 1640),
                       SIMD4(-1, 1, 1, -1))
        XCTAssertEqual(aspectFitRect(contentWidth: 1180, contentHeight: 820, targetWidth: 2360, targetHeight: 1640),
                       SIMD4(-1, 1, 1, -1), "a scaled-down stream fills the screen")
        XCTAssertEqual(aspectFitRect(contentWidth: 200, contentHeight: 100, targetWidth: 100, targetHeight: 100),
                       SIMD4(-1, 0.5, 1, -0.5), "wider: bars above and below")
        XCTAssertEqual(aspectFitRect(contentWidth: 100, contentHeight: 200, targetWidth: 100, targetHeight: 100),
                       SIMD4(-0.5, 1, 0.5, -1), "taller: bars left and right")
        XCTAssertEqual(aspectFitRect(contentWidth: 0, contentHeight: 0, targetWidth: 10, targetHeight: 10),
                       SIMD4(-1, 1, 1, -1))
    }

    func assertRGB(_ v: SIMD3<Float>, _ r: Float, _ g: Float, _ b: Float, _ what: String,
                   file: StaticString = #filePath, line: UInt = #line) {
        let ok = abs(v.x - r) < 0.01 && abs(v.y - g) < 0.01 && abs(v.z - b) < 0.01
        XCTAssertTrue(ok, "\(what): got \(v), want (\(r), \(g), \(b))", file: file, line: line)
    }

    func testYCbCrToRGB() {
        let bt709 = YCbCrToRGB(.bt709, fullRange: false)
        assertRGB(bt709.rgb(y: 235, cb: 128, cr: 128), 1, 1, 1, "white")
        assertRGB(bt709.rgb(y: 16, cb: 128, cr: 128), 0, 0, 0, "black")
        assertRGB(bt709.rgb(y: 63, cb: 102, cr: 240), 1, 0, 0, "BT.709 red")
        assertRGB(bt709.rgb(y: 173, cb: 42, cr: 26), 0, 1, 0, "BT.709 green")
        assertRGB(bt709.rgb(y: 32, cb: 240, cr: 118), 0, 0, 1, "BT.709 blue")
        assertRGB(YCbCrToRGB(.bt601, fullRange: false).rgb(y: 81, cb: 90, cr: 240), 1, 0, 0, "BT.601 red")
        assertRGB(YCbCrToRGB(.bt709, fullRange: true).rgb(y: 255, cb: 128, cr: 128), 1, 1, 1, "full-range white")
        assertRGB(YCbCrToRGB(.bt709, fullRange: true).rgb(y: 0, cb: 128, cr: 128), 0, 0, 0, "full-range black")
    }

    func testPresentedTimesConvertToTheSessionClock() {
        let a = SessionClock.ns(fromMediaTime: CACurrentMediaTime())
        let b = SessionClock.nowNs()
        XCTAssertLessThan(abs(Int64(b) - Int64(a)), 2_000_000, "Core Animation time and the session clock agree")
    }

    // MARK: GPU (skipped without a Metal device)

    func metalDevice() throws -> MTLDevice {
        guard let d = MTLCreateSystemDefaultDevice() else {
            throw XCTSkip("no Metal device on this machine")
        }
        print("Metal device: \(d.name)")
        return d
    }

    /// An IOSurface NV12 picture filled by `pixel(x, y) -> (Y, Cb, Cr)`;
    /// chroma is taken at even coordinates.
    func makeNV12(width: Int, height: Int, matrix: CFString,
                  _ pixel: (Int, Int) -> (UInt8, UInt8, UInt8)) throws -> CVPixelBuffer {
        let attrs: [CFString: Any] = [
            kCVPixelBufferIOSurfacePropertiesKey: [String: Any](),
            kCVPixelBufferMetalCompatibilityKey: true,
        ]
        var pb: CVPixelBuffer?
        XCTAssertEqual(CVPixelBufferCreate(kCFAllocatorDefault, width, height,
                                           kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange, attrs as CFDictionary, &pb),
                       kCVReturnSuccess)
        let image = try XCTUnwrap(pb)
        CVPixelBufferLockBaseAddress(image, [])
        let y = try XCTUnwrap(CVPixelBufferGetBaseAddressOfPlane(image, 0)).assumingMemoryBound(to: UInt8.self)
        let c = try XCTUnwrap(CVPixelBufferGetBaseAddressOfPlane(image, 1)).assumingMemoryBound(to: UInt8.self)
        let ys = CVPixelBufferGetBytesPerRowOfPlane(image, 0)
        let cs = CVPixelBufferGetBytesPerRowOfPlane(image, 1)
        for row in 0..<height {
            for col in 0..<width { y[row * ys + col] = pixel(col, row).0 }
        }
        for row in 0..<height / 2 {
            for col in 0..<width / 2 {
                let p = pixel(col * 2, row * 2)
                c[row * cs + col * 2] = p.1
                c[row * cs + col * 2 + 1] = p.2
            }
        }
        CVPixelBufferUnlockBaseAddress(image, [])
        CVBufferSetAttachment(image, kCVImageBufferYCbCrMatrixKey, matrix, .shouldPropagate)
        return image
    }

    /// Draws `image` into a BGRA texture and reads it back.
    func render(_ r: VideoRenderer, _ image: CVPixelBuffer, width: Int, height: Int) throws -> [UInt8] {
        let desc = MTLTextureDescriptor.texture2DDescriptor(pixelFormat: .bgra8Unorm, width: width, height: height,
                                                            mipmapped: false)
        desc.usage = [.renderTarget]
        desc.storageMode = .private
        let tex = try XCTUnwrap(r.device.makeTexture(descriptor: desc))
        let cb = try r.encode(image, into: tex)
        let buf = try XCTUnwrap(r.device.makeBuffer(length: width * height * 4, options: .storageModeShared))
        let blit = try XCTUnwrap(cb.makeBlitCommandEncoder())
        blit.copy(from: tex, sourceSlice: 0, sourceLevel: 0, sourceOrigin: MTLOrigin(x: 0, y: 0, z: 0),
                  sourceSize: MTLSize(width: width, height: height, depth: 1), to: buf, destinationOffset: 0,
                  destinationBytesPerRow: width * 4, destinationBytesPerImage: width * height * 4)
        blit.endEncoding()
        cb.commit()
        cb.waitUntilCompleted()
        XCTAssertNil(cb.error)
        return [UInt8](UnsafeBufferPointer(start: buf.contents().assumingMemoryBound(to: UInt8.self),
                                           count: width * height * 4))
    }

    /// (R, G, B) at x, y of a BGRA readback.
    func rgbAt(_ px: [UInt8], width: Int, _ x: Int, _ y: Int) -> [Int] {
        let i = (y * width + x) * 4
        return [Int(px[i + 2]), Int(px[i + 1]), Int(px[i])]
    }

    func assertClose(_ got: [Int], _ want: [Int], tolerance: Int = 3, _ what: String,
                     file: StaticString = #filePath, line: UInt = #line) {
        let ok = zip(got, want).allSatisfy { abs($0 - $1) <= tolerance }
        XCTAssertTrue(ok, "\(what): got \(got), want \(want)", file: file, line: line)
    }

    func testShaderConvertsNV12AndFitsTheAspect() throws {
        let r = try VideoRenderer(device: try metalDevice())
        // Four 8×8 quadrants: white, black / BT.709 red, green.
        let image = try makeNV12(width: 16, height: 16, matrix: kCVImageBufferYCbCrMatrix_ITU_R_709_2) { x, y in
            switch (x < 8, y < 8) {
            case (true, true): return (235, 128, 128)
            case (false, true): return (16, 128, 128)
            case (true, false): return (63, 102, 240)
            case (false, false): return (173, 42, 26)
            }
        }
        let px = try render(r, image, width: 16, height: 16)
        assertClose(rgbAt(px, width: 16, 4, 4), [255, 255, 255], "white")
        assertClose(rgbAt(px, width: 16, 11, 4), [0, 0, 0], "black")
        assertClose(rgbAt(px, width: 16, 4, 11), [255, 0, 0], "red")
        assertClose(rgbAt(px, width: 16, 11, 11), [0, 255, 0], "green")

        // Into a 2:1 target: the 16×16 picture sits in the middle, black bars
        // left and right, top-left quadrant stays top-left.
        let wide = try render(r, image, width: 32, height: 16)
        assertClose(rgbAt(wide, width: 32, 2, 8), [0, 0, 0], "left bar")
        assertClose(rgbAt(wide, width: 32, 29, 8), [0, 0, 0], "right bar")
        assertClose(rgbAt(wide, width: 32, 12, 4), [255, 255, 255], "picture, top-left")
        assertClose(rgbAt(wide, width: 32, 12, 11), [255, 0, 0], "picture, bottom-left")
    }

    func testDecodedFrameRendersLikeTheCPUReference() throws {
        let r = try VideoRenderer(device: try metalDevice())
        let (config, frames) = try vector()
        let decoded = Locked<[DecodedVideoFrame]>([])
        let decoder = LowLatencyDecoder { f in decoded.mutate { $0.append(f) } }
        try decoder.configure(config)
        decoder.decode(frames[0], receivedNs: 1) { _ in }
        decoder.waitForPendingFrames()
        let image = try XCTUnwrap(decoded.value.first).image
        let (w, h) = (CVPixelBufferGetWidth(image), CVPixelBufferGetHeight(image))
        let px = try render(r, image, width: w, height: h)

        // CPU: same matrix, chroma sampled bilinearly like the GPU's sampler.
        let conv = YCbCrToRGB.forImage(image)
        CVPixelBufferLockBaseAddress(image, .readOnly)
        defer { CVPixelBufferUnlockBaseAddress(image, .readOnly) }
        let y = try XCTUnwrap(CVPixelBufferGetBaseAddressOfPlane(image, 0)).assumingMemoryBound(to: UInt8.self)
        let c = try XCTUnwrap(CVPixelBufferGetBaseAddressOfPlane(image, 1)).assumingMemoryBound(to: UInt8.self)
        let ys = CVPixelBufferGetBytesPerRowOfPlane(image, 0)
        let cs = CVPixelBufferGetBytesPerRowOfPlane(image, 1)
        let (cw, ch) = (CVPixelBufferGetWidthOfPlane(image, 1), CVPixelBufferGetHeightOfPlane(image, 1))
        func chroma(_ cx: Int, _ cy: Int, _ k: Int) -> Float {
            Float(c[min(max(cy, 0), ch - 1) * cs + min(max(cx, 0), cw - 1) * 2 + k])
        }
        var maxDiff = 0
        for row in 0..<h {
            for col in 0..<w {
                let u = (Float(col) + 0.5) / Float(w) * Float(cw) - 0.5
                let v = (Float(row) + 0.5) / Float(h) * Float(ch) - 0.5
                let (x0, y0) = (Int(u.rounded(.down)), Int(v.rounded(.down)))
                let (fx, fy) = (u - u.rounded(.down), v - v.rounded(.down))
                var cc: [Float] = [0, 0]
                for k in 0..<2 {
                    let top = chroma(x0, y0, k) * (1 - fx) + chroma(x0 + 1, y0, k) * fx
                    let bottom = chroma(x0, y0 + 1, k) * (1 - fx) + chroma(x0 + 1, y0 + 1, k) * fx
                    cc[k] = top * (1 - fy) + bottom * fy
                }
                let want = conv.rgb(y: y[row * ys + col], cb: cc[0], cr: cc[1]) * 255
                let got = rgbAt(px, width: w, col, row)
                for (g, wv) in zip(got, [want.x, want.y, want.z]) {
                    maxDiff = max(maxDiff, abs(g - Int(wv.rounded())))
                }
            }
        }
        print("decoded \(w)×\(h) frame: GPU vs CPU max difference \(maxDiff)/255")
        XCTAssertLessThanOrEqual(maxDiff, 4)
    }

    /// The whole Metal sink with an offscreen CAMetalLayer, in one
    /// presentation mode (optionally switching to another halfway). Whether
    /// macOS presents a layer that isn't in a window, or runs a display link
    /// for it, is up to the runner, so this checks the bookkeeping (every
    /// frame reported exactly once, consistent times, layer settings) and
    /// prints what happened; presentation proper needs an iPad.
    func exerciseSink(_ mode: MetalPresentMode, switchTo later: [MetalPresentMode] = []) throws {
        let device = try metalDevice()
        let (config, frames) = try vector()
        let layer = CAMetalLayer()
        let errors = Locked<[String]>([])
        let stats = Locked<[String]>([])
        let sink = MetalVideoSink(layer: layer, device: device, mode: mode,
                                  onError: { e in errors.mutate { $0.append(e) } },
                                  onStats: { s in stats.mutate { $0.append(s) } })
        func checkLayer(_ m: MetalPresentMode) {
            XCTAssertEqual(sink.mode, m)
            XCTAssertEqual(layer.maximumDrawableCount, m == .immediate ? 2 : 3)
            XCTAssertEqual(layer.presentsWithTransaction, m == .uiUpdate)
        }
        checkLayer(mode)
        layer.drawableSize = CGSize(width: 176, height: 144)
        let log = FeedbackLog()
        sink.configure(config)
        // Switch every couple of frames, through `later` in order.
        let step = later.isEmpty ? Int.max : max(1, frames.count / (later.count + 1))
        for (i, f) in frames.enumerated() {
            if i > 0, i % step == 0, i / step <= later.count {
                let next = later[i / step - 1]
                sink.setMode(next)
                checkLayer(next)
            }
            sink.enqueue(f, receivedNs: SessionClock.nowNs(), report: log.add)
            usleep(20_000)
            if sink.mode == .uiUpdate {
                // What the iPad's UIUpdateLink does once per refresh, then the
                // UI update's Core Animation commit.
                sink.presentForUIUpdate(target: CACurrentMediaTime() + 0.017, immediateExpected: false)
                CATransaction.flush()
            }
        }
        usleep(200_000)
        sink.stop()
        let deadline = Date().addingTimeInterval(5)
        while log.all.count < frames.count && Date() < deadline { usleep(10_000) }
        let fb = log.all
        let shown = fb.filter { $0.displayedNs != 0 }
        let name = ([mode] + later).map(\.rawValue).joined(separator: " -> ")
        print("Metal sink (\(name)): \(fb.count)/\(frames.count) reported, \(shown.count) presented, " +
              "errors \(errors.value), readouts \(stats.value)")
        XCTAssertEqual(Set(fb.map(\.frameID)).count, fb.count, "no frame reported twice")
        // Drawables still on their way to the (absent) screen may hold the
        // last frames; everything else must have been reported.
        XCTAssertGreaterThanOrEqual(fb.count, frames.count - 2)
        for x in fb where !x.requestKeyframe {
            XCTAssertGreaterThanOrEqual(x.decodedNs, x.receivedNs)
            if x.displayedNs != 0 { XCTAssertGreaterThanOrEqual(x.displayedNs, x.decodedNs) }
        }
    }

    func testMetalSinkDisplayLinkMode() throws { try exerciseSink(.displayLink) }
    func testMetalSinkUIUpdateMode() throws { try exerciseSink(.uiUpdate) }
    func testMetalSinkImmediateMode() throws { try exerciseSink(.immediate) }
    /// Every transition into and out of the display link (the layer refuses
    /// some changes while a display link drives it).
    func testMetalSinkSwitchesModesMidStream() throws {
        try exerciseSink(.displayLink, switchTo: [.immediate, .displayLink, .uiUpdate, .displayLink])
    }

    // MARK: on-iPad readout

    func testPresentWindowSummary() throws {
        var w = PresentWindow()
        XCTAssertNil(w.summary(mode: .displayLink, seconds: 1))
        let ms: UInt64 = 1_000_000
        for i in 0..<10 {
            let d = 100 * ms + UInt64(i) * 16 * ms
            w.add(decodedNs: d, submittedNs: d + 2 * ms, displayedNs: d + 20 * ms, targetNs: d + 20 * ms,
                  immediateExpected: false)
        }
        // One late frame (20 ms after its target), one never shown.
        w.add(decodedNs: 1000 * ms, submittedNs: 1002 * ms, displayedNs: 1040 * ms, targetNs: 1020 * ms,
              immediateExpected: false)
        w.add(decodedNs: 5, submittedNs: 6, displayedNs: 0, targetNs: nil, immediateExpected: false)
        XCTAssertEqual(w.shown, 11)
        XCTAssertEqual(w.dropped, 1)
        let s = try XCTUnwrap(w.summary(mode: .displayLink, seconds: 1))
        XCTAssertTrue(s.hasPrefix("Metal · display link: decoded → screen 20.0/40.0 ms"), s)
        XCTAssertTrue(s.contains("submit → screen 18.0 ms"), s)
        XCTAssertTrue(s.contains("11 shown/s"), s)
        XCTAssertTrue(s.contains("1 dropped"), s)
        XCTAssertTrue(s.contains("on target 90%"), s)
        XCTAssertFalse(s.contains("immediate"), s)

        var u = PresentWindow()
        u.add(decodedNs: 0, submittedNs: 0, displayedNs: 16 * ms, targetNs: nil, immediateExpected: true)
        u.add(decodedNs: 0, submittedNs: 0, displayedNs: 16 * ms, targetNs: nil, immediateExpected: false)
        let us = try XCTUnwrap(u.summary(mode: .uiUpdate, seconds: 2))
        XCTAssertTrue(us.hasPrefix("Metal · UI update link: decoded → screen 16.0/16.0 ms"), us)
        XCTAssertTrue(us.contains("1 shown/s"), us)
        XCTAssertTrue(us.contains("immediate 50%"), us)
        XCTAssertFalse(us.contains("on target"), us)
    }
}

// Decodes, with Apple's VideoToolbox, the video messages the Rust daemon
// produced from an x264 clip (protocol/vectors/video-176x144.frames, made
// by daemon/tests/video_vectors.rs). This checks the AVCC framing, the
// parameter-set handling and the protocol codec across both implementations.
import CoreMedia
import CoreVideo
import Foundation
import SidecarProtocol
import XCTest

@testable import SidecarNet

final class VideoDecoderTests: XCTestCase {
    static func messages() throws -> [Message] {
        let url = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()  // SidecarNetTests
            .deletingLastPathComponent()  // Tests
            .deletingLastPathComponent()  // ios-app
            .deletingLastPathComponent()  // repo
            .appendingPathComponent("protocol/vectors/video-176x144.frames")
        var d = FrameDecoder()
        d.push([UInt8](try Data(contentsOf: url)))
        var out: [Message] = []
        while let f = try d.nextFrame() {
            out.append(try Message.decode(type: f.type, payload: f.payload))
        }
        return out
    }

    func testDecodesRustProducedH264Frames() throws {
        let msgs = try Self.messages()
        XCTAssertEqual(msgs.count, 9)
        let decoder = VideoDecoder()
        var decoded = 0
        var keyframes = 0
        for m in msgs {
            switch m {
            case .videoConfig(let c):
                XCTAssertEqual(c.codec, .h264)
                try decoder.configure(c)
                let dims = try XCTUnwrap(decoder.currentFormat).dimensions
                XCTAssertEqual(dims.width, 176)
                XCTAssertEqual(dims.height, 144)
            case .videoFrame(let f):
                if f.keyframe { keyframes += 1 }
                let image = try XCTUnwrap(try decoder.decode(f), "frame \(f.frameID) decoded to nothing")
                XCTAssertEqual(CVPixelBufferGetWidth(image), 176)
                XCTAssertEqual(CVPixelBufferGetHeight(image), 144)
                decoded += 1
            default:
                XCTFail("unexpected \(m)")
            }
        }
        XCTAssertEqual(decoded, 8)
        XCTAssertEqual(keyframes, 2)
    }

    func testSampleBuffersAreMarkedDisplayImmediately() throws {
        let msgs = try Self.messages()
        guard case .videoConfig(let c) = msgs[0], case .videoFrame(let f) = msgs[1] else {
            return XCTFail("unexpected vector layout")
        }
        let format = try VideoFormat(c)
        let sample = try format.sampleBuffer(f)
        XCTAssertEqual(CMSampleBufferGetTotalSampleSize(sample), f.data.count)
        let attachments = try XCTUnwrap(CMSampleBufferGetSampleAttachmentsArray(sample, createIfNecessary: false))
        let dict = unsafeBitCast(CFArrayGetValueAtIndex(attachments, 0), to: CFDictionary.self) as NSDictionary
        XCTAssertEqual(dict[kCMSampleAttachmentKey_DisplayImmediately as String] as? Bool, true)
    }

    func testRejectsMissingParameterSets() {
        let bad = VideoConfig(codec: .h264, width: 1, height: 1, fpsMilli: 1, parameterSets: [])
        XCTAssertThrowsError(try VideoFormat(bad))
    }
}

// Phase 2, receive side: protocol VideoConfig/VideoFrame → CoreMedia.
//
// Frames arrive as AVCC/HVCC samples (4-byte big-endian NAL lengths) and
// parameter sets arrive separately, so no Annex-B conversion is needed here.
// `VideoFormat.sampleBuffer` feeds AVSampleBufferDisplayLayer (lowest
// latency, DisplayImmediately); `VideoDecoder` decodes with
// VTDecompressionSession (used by tests and for latency instrumentation).
import CoreMedia
import CoreVideo
import Foundation
import SidecarProtocol
import VideoToolbox

public enum VideoError: Error, Equatable {
    case noParameterSets
    case formatDescription(OSStatus)
    case blockBuffer(OSStatus)
    case sampleBuffer(OSStatus)
    case session(OSStatus)
    case decode(OSStatus)
    case notConfigured
    case emptyFrame
}

public final class VideoFormat {
    public let config: VideoConfig
    public let formatDescription: CMVideoFormatDescription

    public init(_ config: VideoConfig) throws {
        guard !config.parameterSets.isEmpty, config.parameterSets.allSatisfy({ !$0.isEmpty }) else {
            throw VideoError.noParameterSets
        }
        // Copy the parameter sets into stable memory for the C call.
        let buffers: [UnsafeMutableBufferPointer<UInt8>] = config.parameterSets.map { set in
            let b = UnsafeMutableBufferPointer<UInt8>.allocate(capacity: set.count)
            _ = b.initialize(from: set)
            return b
        }
        defer { buffers.forEach { $0.deallocate() } }
        let pointers: [UnsafePointer<UInt8>] = buffers.map { UnsafePointer($0.baseAddress!) }
        let sizes: [Int] = buffers.map { $0.count }
        var fd: CMFormatDescription?
        let status: OSStatus
        switch config.codec {
        case .h264:
            status = CMVideoFormatDescriptionCreateFromH264ParameterSets(
                allocator: kCFAllocatorDefault, parameterSetCount: pointers.count,
                parameterSetPointers: pointers, parameterSetSizes: sizes,
                nalUnitHeaderLength: 4, formatDescriptionOut: &fd)
        case .hevc:
            status = CMVideoFormatDescriptionCreateFromHEVCParameterSets(
                allocator: kCFAllocatorDefault, parameterSetCount: pointers.count,
                parameterSetPointers: pointers, parameterSetSizes: sizes,
                nalUnitHeaderLength: 4, extensions: nil, formatDescriptionOut: &fd)
        }
        guard status == noErr, let fd else { throw VideoError.formatDescription(status) }
        self.config = config
        self.formatDescription = fd
    }

    /// Pixel size from the parameter sets (what the decoder will output).
    public var dimensions: CMVideoDimensions {
        CMVideoFormatDescriptionGetDimensions(formatDescription)
    }

    /// One frame as a CMSampleBuffer. `displayImmediately` marks it for
    /// AVSampleBufferDisplayLayer to show as soon as it is decoded,
    /// ignoring timestamps (we want minimum latency, not pacing).
    public func sampleBuffer(_ frame: VideoFrame, displayImmediately: Bool = true) throws -> CMSampleBuffer {
        guard !frame.data.isEmpty else { throw VideoError.emptyFrame }
        var block: CMBlockBuffer?
        var status = CMBlockBufferCreateWithMemoryBlock(
            allocator: kCFAllocatorDefault, memoryBlock: nil, blockLength: frame.data.count,
            blockAllocator: kCFAllocatorDefault, customBlockSource: nil, offsetToData: 0,
            dataLength: frame.data.count, flags: 0, blockBufferOut: &block)
        guard status == kCMBlockBufferNoErr, let block else { throw VideoError.blockBuffer(status) }
        status = frame.data.withUnsafeBytes { raw in
            CMBlockBufferReplaceDataBytes(with: raw.baseAddress!, blockBuffer: block, offsetIntoDestination: 0,
                                          dataLength: frame.data.count)
        }
        guard status == kCMBlockBufferNoErr else { throw VideoError.blockBuffer(status) }

        var sample: CMSampleBuffer?
        var size = frame.data.count
        var timing = CMSampleTimingInfo(
            duration: .invalid,
            presentationTimeStamp: CMTime(value: CMTimeValue(frame.captureNs), timescale: 1_000_000_000),
            decodeTimeStamp: .invalid)
        status = CMSampleBufferCreateReady(
            allocator: kCFAllocatorDefault, dataBuffer: block, formatDescription: formatDescription,
            sampleCount: 1, sampleTimingEntryCount: 1, sampleTimingArray: &timing,
            sampleSizeEntryCount: 1, sampleSizeArray: &size, sampleBufferOut: &sample)
        guard status == noErr, let sample else { throw VideoError.sampleBuffer(status) }
        if displayImmediately,
           let attachments = CMSampleBufferGetSampleAttachmentsArray(sample, createIfNecessary: true),
           CFArrayGetCount(attachments) > 0 {
            let dict = unsafeBitCast(CFArrayGetValueAtIndex(attachments, 0), to: CFMutableDictionary.self)
            CFDictionarySetValue(dict,
                                 Unmanaged.passUnretained(kCMSampleAttachmentKey_DisplayImmediately).toOpaque(),
                                 Unmanaged.passUnretained(kCFBooleanTrue).toOpaque())
        }
        return sample
    }
}

/// Synchronous VideoToolbox decoder.
public final class VideoDecoder {
    private var format: VideoFormat?
    private var session: VTDecompressionSession?

    public init() {}

    public var currentFormat: VideoFormat? { format }

    public func configure(_ config: VideoConfig) throws {
        let f = try VideoFormat(config)
        if let s = session {
            VTDecompressionSessionInvalidate(s)
            session = nil
        }
        var s: VTDecompressionSession?
        let attrs: [CFString: Any] = [
            kCVPixelBufferPixelFormatTypeKey: kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
        ]
        let status = VTDecompressionSessionCreate(
            allocator: kCFAllocatorDefault, formatDescription: f.formatDescription,
            decoderSpecification: nil, imageBufferAttributes: attrs as CFDictionary,
            outputCallback: nil, decompressionSessionOut: &s)
        guard status == noErr, let s else { throw VideoError.session(status) }
        session = s
        format = f
    }

    /// Decodes one frame and returns the image (nil if the decoder dropped it).
    public func decode(_ frame: VideoFrame) throws -> CVImageBuffer? {
        guard let session, let format else { throw VideoError.notConfigured }
        let sample = try format.sampleBuffer(frame, displayImmediately: false)
        final class Box: @unchecked Sendable {
            var status: OSStatus = noErr
            var image: CVImageBuffer?
        }
        let box = Box()
        let status = VTDecompressionSessionDecodeFrame(
            session, sampleBuffer: sample, flags: [], infoFlagsOut: nil
        ) { st, _, image, _, _ in
            box.status = st
            box.image = image
        }
        guard status == noErr else { throw VideoError.decode(status) }
        VTDecompressionSessionWaitForAsynchronousFrames(session)
        guard box.status == noErr else { throw VideoError.decode(box.status) }
        return box.image
    }

    deinit {
        if let session { VTDecompressionSessionInvalidate(session) }
    }
}

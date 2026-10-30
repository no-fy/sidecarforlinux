// Message definitions mirroring protocol/rust/src/messages.rs.
// Field order is wire order; see docs/protocol.md.

public enum SidecarProtocol {
    public static let major: UInt16 = 1
    public static let minor: UInt16 = 0
    public static let helloMagic: [UInt8] = Array("SCLP".utf8)
    public static let defaultPort: UInt16 = 47811
    public static let bonjourServiceType = "_sidecarlinux._tcp"
    public static let alpn = "sidecar/1"
    public static let maxFrameLength = 16 * 1024 * 1024
}

public enum MessageType {
    public static let hello: UInt8 = 0x01
    public static let helloAck: UInt8 = 0x02
    public static let pairCommit: UInt8 = 0x03
    public static let pairNonce: UInt8 = 0x04
    public static let pairReveal: UInt8 = 0x05
    public static let pairDecision: UInt8 = 0x06
    public static let goodbye: UInt8 = 0x07
    public static let penBatch: UInt8 = 0x10
    public static let pencilGesture: UInt8 = 0x11
    public static let surfaceInfo: UInt8 = 0x12
    public static let haptic: UInt8 = 0x20
    public static let mappingInfo: UInt8 = 0x21
    public static let ping: UInt8 = 0x30
    public static let pong: UInt8 = 0x31
    public static let latencyReport: UInt8 = 0x32
    public static let videoConfig: UInt8 = 0x40
    public static let videoFrame: UInt8 = 0x41
    public static let videoFeedback: UInt8 = 0x42
}

public enum Caps {
    public static let gestures: UInt32 = 1 << 0
    public static let haptics: UInt32 = 1 << 1
    public static let video: UInt32 = 1 << 2
    public static let predicted: UInt32 = 1 << 3
    public static let roll: UInt32 = 1 << 4
    public static let hover: UInt32 = 1 << 5
    public static let hevc: UInt32 = 1 << 6
}

public enum Channel: UInt8, Sendable {
    case input = 0
    case video = 1
}

public struct Hello: Equatable, Sendable {
    public var major: UInt16
    public var minor: UInt16
    public var channel: Channel
    public var trustsPeer: Bool
    public var caps: UInt32
    public var sessionID: UInt64
    public var name: String

    public init(major: UInt16 = SidecarProtocol.major, minor: UInt16 = SidecarProtocol.minor, channel: Channel,
                trustsPeer: Bool, caps: UInt32, sessionID: UInt64, name: String) {
        self.major = major
        self.minor = minor
        self.channel = channel
        self.trustsPeer = trustsPeer
        self.caps = caps
        self.sessionID = sessionID
        self.name = name
    }
}

public struct SurfaceInfo: Equatable, Sendable {
    public var widthPt: Float
    public var heightPt: Float
    public var scale: Float
    public var pointsPerInch: Float

    public init(widthPt: Float, heightPt: Float, scale: Float, pointsPerInch: Float) {
        self.widthPt = widthPt
        self.heightPt = heightPt
        self.scale = scale
        self.pointsPerInch = pointsPerInch
    }
}

public struct HelloAck: Equatable, Sendable {
    public var major: UInt16
    public var minor: UInt16
    public var trustsPeer: Bool
    public var caps: UInt32
    public var surface: SurfaceInfo
    public var deviceName: String
    public var model: String

    public init(major: UInt16 = SidecarProtocol.major, minor: UInt16 = SidecarProtocol.minor, trustsPeer: Bool,
                caps: UInt32, surface: SurfaceInfo, deviceName: String, model: String) {
        self.major = major
        self.minor = minor
        self.trustsPeer = trustsPeer
        self.caps = caps
        self.surface = surface
        self.deviceName = deviceName
        self.model = model
    }
}

public enum PenPhase: UInt8, Sendable {
    case hover = 0, down = 1, move = 2, up = 3, cancel = 4, out = 5
}

public enum SampleFlags {
    public static let predicted: UInt8 = 1 << 0
    public static let coalesced: UInt8 = 1 << 1
    public static let pressureEstimated: UInt8 = 1 << 2
    public static let anglesEstimated: UInt8 = 1 << 3
    public static let rollValid: UInt8 = 1 << 4
    public static let fromHover: UInt8 = 1 << 5
}

public struct PenSample: Equatable, Sendable {
    public static let wireLength = 48
    /// Session clock in ns (UITouch.timestamp − session epoch).
    public var timestampNs: UInt64
    public var seq: UInt32
    public var phase: PenPhase
    public var flags: UInt8
    public var x: Float
    public var y: Float
    public var pressure: Float
    public var altitude: Float
    public var azimuth: Float
    public var roll: Float
    public var zOffset: Float
    public var estimationIndex: UInt32

    public init(timestampNs: UInt64, seq: UInt32, phase: PenPhase, flags: UInt8, x: Float, y: Float,
                pressure: Float, altitude: Float, azimuth: Float, roll: Float, zOffset: Float,
                estimationIndex: UInt32) {
        self.timestampNs = timestampNs
        self.seq = seq
        self.phase = phase
        self.flags = flags
        self.x = x
        self.y = y
        self.pressure = pressure
        self.altitude = altitude
        self.azimuth = azimuth
        self.roll = roll
        self.zOffset = zOffset
        self.estimationIndex = estimationIndex
    }

    func write(_ w: inout ByteWriter) {
        w.u64(timestampNs)
        w.u32(seq)
        w.u8(phase.rawValue)
        w.u8(flags)
        w.u16(0)
        w.f32(x)
        w.f32(y)
        w.f32(pressure)
        w.f32(altitude)
        w.f32(azimuth)
        w.f32(roll)
        w.f32(zOffset)
        w.u32(estimationIndex)
    }

    static func read(_ r: inout ByteReader) throws -> PenSample {
        let ts = try r.u64()
        let seq = try r.u32()
        let rawPhase = try r.u8()
        guard let phase = PenPhase(rawValue: rawPhase) else {
            throw ProtocolError.badValue(field: "phase", value: UInt32(rawPhase))
        }
        let flags = try r.u8()
        _ = try r.u16()
        return PenSample(timestampNs: ts, seq: seq, phase: phase, flags: flags, x: try r.f32(), y: try r.f32(),
                         pressure: try r.f32(), altitude: try r.f32(), azimuth: try r.f32(), roll: try r.f32(),
                         zOffset: try r.f32(), estimationIndex: try r.u32())
    }
}

public struct PenBatch: Equatable, Sendable {
    public var sendTimeNs: UInt64
    public var samples: [PenSample]

    public init(sendTimeNs: UInt64, samples: [PenSample]) {
        self.sendTimeNs = sendTimeNs
        self.samples = samples
    }
}

public enum GestureKind: UInt8, Sendable { case doubleTap = 1, squeeze = 2 }
public enum GesturePhase: UInt8, Sendable { case discrete = 0, began = 1, changed = 2, ended = 3, cancelled = 4 }

public struct HoverPose: Equatable, Sendable {
    public var x: Float
    public var y: Float
    public var zOffset: Float
    public var altitude: Float
    public var azimuth: Float
    public var roll: Float

    public init(x: Float, y: Float, zOffset: Float, altitude: Float, azimuth: Float, roll: Float) {
        self.x = x
        self.y = y
        self.zOffset = zOffset
        self.altitude = altitude
        self.azimuth = azimuth
        self.roll = roll
    }
}

public struct PencilGesture: Equatable, Sendable {
    public var timestampNs: UInt64
    public var kind: GestureKind
    public var phase: GesturePhase
    public var preferredAction: UInt8
    public var hoverPose: HoverPose?

    public init(timestampNs: UInt64, kind: GestureKind, phase: GesturePhase, preferredAction: UInt8,
                hoverPose: HoverPose?) {
        self.timestampNs = timestampNs
        self.kind = kind
        self.phase = phase
        self.preferredAction = preferredAction
        self.hoverPose = hoverPose
    }
}

public enum HapticKind: UInt8, Sendable { case alignment = 1, pathCompleted = 2 }

public struct Haptic: Equatable, Sendable {
    public var kind: HapticKind
    /// Normalised location; NaN = where the pencil is.
    public var x: Float
    public var y: Float

    public init(kind: HapticKind, x: Float, y: Float) {
        self.kind = kind
        self.x = x
        self.y = y
    }
}

public struct MappingInfo: Equatable, Sendable {
    public var outputName: String
    public var area: [Float]

    public init(outputName: String, area: [Float]) {
        self.outputName = outputName
        self.area = area
    }
}

public struct Ping: Equatable, Sendable {
    public var id: UInt32
    public var t1Ns: UInt64
    public init(id: UInt32, t1Ns: UInt64) {
        self.id = id
        self.t1Ns = t1Ns
    }
}

public struct Pong: Equatable, Sendable {
    public var id: UInt32
    public var t1Ns: UInt64
    public var t2Ns: UInt64
    public var t3Ns: UInt64
    public init(id: UInt32, t1Ns: UInt64, t2Ns: UInt64, t3Ns: UInt64) {
        self.id = id
        self.t1Ns = t1Ns
        self.t2Ns = t2Ns
        self.t3Ns = t3Ns
    }
}

public struct LatencyReport: Equatable, Sendable {
    public var samples: UInt32
    public var p50Us: UInt32
    public var p95Us: UInt32
    public var p99Us: UInt32
    public var maxUs: UInt32
    public var rttUs: UInt32
    public init(samples: UInt32, p50Us: UInt32, p95Us: UInt32, p99Us: UInt32, maxUs: UInt32, rttUs: UInt32) {
        self.samples = samples
        self.p50Us = p50Us
        self.p95Us = p95Us
        self.p99Us = p99Us
        self.maxUs = maxUs
        self.rttUs = rttUs
    }
}

public enum GoodbyeCode: UInt16, Sendable {
    case normal = 0, versionMismatch = 1, notPaired = 2, pairingRejected = 3, protocolError = 4, busy = 5,
         shuttingDown = 6
}

public enum VideoCodec: UInt8, Sendable { case h264 = 1, hevc = 2 }

public struct VideoConfig: Equatable, Sendable {
    public var codec: VideoCodec
    public var width: UInt32
    public var height: UInt32
    public var fpsMilli: UInt32
    public var parameterSets: [[UInt8]]
    public init(codec: VideoCodec, width: UInt32, height: UInt32, fpsMilli: UInt32, parameterSets: [[UInt8]]) {
        self.codec = codec
        self.width = width
        self.height = height
        self.fpsMilli = fpsMilli
        self.parameterSets = parameterSets
    }
}

public struct VideoFrame: Equatable, Sendable {
    public var frameID: UInt32
    public var keyframe: Bool
    public var captureNs: UInt64
    public var encodedNs: UInt64
    /// AVCC/HVCC sample: 4-byte big-endian length-prefixed NAL units.
    public var data: [UInt8]
    public init(frameID: UInt32, keyframe: Bool, captureNs: UInt64, encodedNs: UInt64, data: [UInt8]) {
        self.frameID = frameID
        self.keyframe = keyframe
        self.captureNs = captureNs
        self.encodedNs = encodedNs
        self.data = data
    }
}

public struct VideoFeedback: Equatable, Sendable {
    public var frameID: UInt32
    public var requestKeyframe: Bool
    public var receivedNs: UInt64
    public var decodedNs: UInt64
    public var displayedNs: UInt64
    public init(frameID: UInt32, requestKeyframe: Bool, receivedNs: UInt64, decodedNs: UInt64,
                displayedNs: UInt64) {
        self.frameID = frameID
        self.requestKeyframe = requestKeyframe
        self.receivedNs = receivedNs
        self.decodedNs = decodedNs
        self.displayedNs = displayedNs
    }
}

public enum Message: Equatable, Sendable {
    case hello(Hello)
    case helloAck(HelloAck)
    case pairCommit([UInt8])
    case pairNonce([UInt8])
    case pairReveal([UInt8])
    case pairDecision(Bool)
    case goodbye(code: GoodbyeCode, reason: String)
    case penBatch(PenBatch)
    case pencilGesture(PencilGesture)
    case surfaceInfo(SurfaceInfo)
    case haptic(Haptic)
    case mappingInfo(MappingInfo)
    case ping(Ping)
    case pong(Pong)
    case latencyReport(LatencyReport)
    case videoConfig(VideoConfig)
    case videoFrame(VideoFrame)
    case videoFeedback(VideoFeedback)
    case unknown(type: UInt8, payload: [UInt8])

    public var typeByte: UInt8 {
        switch self {
        case .hello: return MessageType.hello
        case .helloAck: return MessageType.helloAck
        case .pairCommit: return MessageType.pairCommit
        case .pairNonce: return MessageType.pairNonce
        case .pairReveal: return MessageType.pairReveal
        case .pairDecision: return MessageType.pairDecision
        case .goodbye: return MessageType.goodbye
        case .penBatch: return MessageType.penBatch
        case .pencilGesture: return MessageType.pencilGesture
        case .surfaceInfo: return MessageType.surfaceInfo
        case .haptic: return MessageType.haptic
        case .mappingInfo: return MessageType.mappingInfo
        case .ping: return MessageType.ping
        case .pong: return MessageType.pong
        case .latencyReport: return MessageType.latencyReport
        case .videoConfig: return MessageType.videoConfig
        case .videoFrame: return MessageType.videoFrame
        case .videoFeedback: return MessageType.videoFeedback
        case .unknown(let t, _): return t
        }
    }

    private static func writeSurface(_ w: inout ByteWriter, _ s: SurfaceInfo) {
        w.f32(s.widthPt)
        w.f32(s.heightPt)
        w.f32(s.scale)
        w.f32(s.pointsPerInch)
    }

    private static func readSurface(_ r: inout ByteReader) throws -> SurfaceInfo {
        SurfaceInfo(widthPt: try r.f32(), heightPt: try r.f32(), scale: try r.f32(), pointsPerInch: try r.f32())
    }

    public func encodePayload() -> [UInt8] {
        var w = ByteWriter()
        switch self {
        case .hello(let h):
            w.raw(SidecarProtocol.helloMagic)
            w.u16(h.major)
            w.u16(h.minor)
            w.u8(h.channel.rawValue)
            w.bool(h.trustsPeer)
            w.u16(0)
            w.u32(h.caps)
            w.u64(h.sessionID)
            w.string(h.name)
        case .helloAck(let a):
            w.u16(a.major)
            w.u16(a.minor)
            w.bool(a.trustsPeer)
            w.u8(0)
            w.u16(0)
            w.u32(a.caps)
            Message.writeSurface(&w, a.surface)
            w.string(a.deviceName)
            w.string(a.model)
        case .pairCommit(let b), .pairNonce(let b), .pairReveal(let b):
            w.raw(b)
        case .pairDecision(let ok):
            w.bool(ok)
        case .goodbye(let code, let reason):
            w.u16(code.rawValue)
            w.string(reason)
        case .penBatch(let b):
            w.u64(b.sendTimeNs)
            w.u16(UInt16(b.samples.count))
            w.u16(UInt16(PenSample.wireLength))
            for s in b.samples { s.write(&w) }
        case .pencilGesture(let g):
            w.u64(g.timestampNs)
            w.u8(g.kind.rawValue)
            w.u8(g.phase.rawValue)
            w.bool(g.hoverPose != nil)
            w.u8(g.preferredAction)
            let p = g.hoverPose ?? HoverPose(x: 0, y: 0, zOffset: 0, altitude: 0, azimuth: 0, roll: 0)
            w.f32(p.x)
            w.f32(p.y)
            w.f32(p.zOffset)
            w.f32(p.altitude)
            w.f32(p.azimuth)
            w.f32(p.roll)
        case .surfaceInfo(let s):
            Message.writeSurface(&w, s)
        case .haptic(let h):
            w.u8(h.kind.rawValue)
            w.u8(0)
            w.u16(0)
            w.f32(h.x)
            w.f32(h.y)
        case .mappingInfo(let m):
            for i in 0..<4 { w.f32(i < m.area.count ? m.area[i] : 0) }
            w.string(m.outputName)
        case .ping(let p):
            w.u32(p.id)
            w.u64(p.t1Ns)
        case .pong(let p):
            w.u32(p.id)
            w.u64(p.t1Ns)
            w.u64(p.t2Ns)
            w.u64(p.t3Ns)
        case .latencyReport(let l):
            w.u32(l.samples)
            w.u32(l.p50Us)
            w.u32(l.p95Us)
            w.u32(l.p99Us)
            w.u32(l.maxUs)
            w.u32(l.rttUs)
        case .videoConfig(let v):
            w.u8(v.codec.rawValue)
            w.u8(UInt8(v.parameterSets.count))
            w.u16(0)
            w.u32(v.width)
            w.u32(v.height)
            w.u32(v.fpsMilli)
            for ps in v.parameterSets { w.blob16(ps) }
        case .videoFrame(let f):
            w.u32(f.frameID)
            w.bool(f.keyframe)
            w.u8(0)
            w.u16(0)
            w.u64(f.captureNs)
            w.u64(f.encodedNs)
            w.raw(f.data)
        case .videoFeedback(let f):
            w.u32(f.frameID)
            w.bool(f.requestKeyframe)
            w.u8(0)
            w.u16(0)
            w.u64(f.receivedNs)
            w.u64(f.decodedNs)
            w.u64(f.displayedNs)
        case .unknown(_, let payload):
            w.raw(payload)
        }
        return w.bytes
    }

    /// Complete frame (header + payload).
    public func encode() -> [UInt8] {
        encodeFrame(type: typeByte, payload: encodePayload())
    }

    public static func decode(type: UInt8, payload: [UInt8]) throws -> Message {
        var r = ByteReader(type: type, data: payload)
        switch type {
        case MessageType.hello:
            guard try r.array(4) == SidecarProtocol.helloMagic else { throw ProtocolError.badMagic }
            let major = try r.u16()
            let minor = try r.u16()
            let rawChannel = try r.u8()
            guard let channel = Channel(rawValue: rawChannel) else {
                throw ProtocolError.badValue(field: "channel", value: UInt32(rawChannel))
            }
            let trusts = try r.u8() != 0
            _ = try r.u16()
            return .hello(Hello(major: major, minor: minor, channel: channel, trustsPeer: trusts, caps: try r.u32(),
                                sessionID: try r.u64(), name: try r.string()))
        case MessageType.helloAck:
            let major = try r.u16()
            let minor = try r.u16()
            let trusts = try r.u8() != 0
            _ = try r.u8()
            _ = try r.u16()
            return .helloAck(HelloAck(major: major, minor: minor, trustsPeer: trusts, caps: try r.u32(),
                                      surface: try readSurface(&r), deviceName: try r.string(),
                                      model: try r.string()))
        case MessageType.pairCommit: return .pairCommit(try r.array(32))
        case MessageType.pairNonce: return .pairNonce(try r.array(32))
        case MessageType.pairReveal: return .pairReveal(try r.array(32))
        case MessageType.pairDecision: return .pairDecision(try r.u8() != 0)
        case MessageType.goodbye:
            let raw = try r.u16()
            return .goodbye(code: GoodbyeCode(rawValue: raw) ?? .normal, reason: try r.string())
        case MessageType.penBatch:
            let send = try r.u64()
            let count = Int(try r.u16())
            let sampleLen = Int(try r.u16())
            guard sampleLen >= PenSample.wireLength else {
                throw ProtocolError.badValue(field: "sample_len", value: UInt32(sampleLen))
            }
            var samples: [PenSample] = []
            samples.reserveCapacity(count)
            for _ in 0..<count {
                let raw = try r.array(sampleLen)
                var sr = ByteReader(type: type, data: Array(raw[0..<PenSample.wireLength]))
                samples.append(try PenSample.read(&sr))
            }
            return .penBatch(PenBatch(sendTimeNs: send, samples: samples))
        case MessageType.pencilGesture:
            let ts = try r.u64()
            let rawKind = try r.u8()
            guard let kind = GestureKind(rawValue: rawKind) else {
                throw ProtocolError.badValue(field: "gesture kind", value: UInt32(rawKind))
            }
            let rawPhase = try r.u8()
            guard let phase = GesturePhase(rawValue: rawPhase) else {
                throw ProtocolError.badValue(field: "gesture phase", value: UInt32(rawPhase))
            }
            let hasPose = (try r.u8() & 1) != 0
            let preferred = try r.u8()
            let pose = HoverPose(x: try r.f32(), y: try r.f32(), zOffset: try r.f32(), altitude: try r.f32(),
                                 azimuth: try r.f32(), roll: try r.f32())
            return .pencilGesture(PencilGesture(timestampNs: ts, kind: kind, phase: phase,
                                                preferredAction: preferred, hoverPose: hasPose ? pose : nil))
        case MessageType.surfaceInfo:
            return .surfaceInfo(try readSurface(&r))
        case MessageType.haptic:
            let rawKind = try r.u8()
            guard let kind = HapticKind(rawValue: rawKind) else {
                throw ProtocolError.badValue(field: "haptic kind", value: UInt32(rawKind))
            }
            _ = try r.u8()
            _ = try r.u16()
            return .haptic(Haptic(kind: kind, x: try r.f32(), y: try r.f32()))
        case MessageType.mappingInfo:
            let area = [try r.f32(), try r.f32(), try r.f32(), try r.f32()]
            return .mappingInfo(MappingInfo(outputName: try r.string(), area: area))
        case MessageType.ping:
            return .ping(Ping(id: try r.u32(), t1Ns: try r.u64()))
        case MessageType.pong:
            return .pong(Pong(id: try r.u32(), t1Ns: try r.u64(), t2Ns: try r.u64(), t3Ns: try r.u64()))
        case MessageType.latencyReport:
            return .latencyReport(LatencyReport(samples: try r.u32(), p50Us: try r.u32(), p95Us: try r.u32(),
                                                p99Us: try r.u32(), maxUs: try r.u32(), rttUs: try r.u32()))
        case MessageType.videoConfig:
            let rawCodec = try r.u8()
            guard let codec = VideoCodec(rawValue: rawCodec) else {
                throw ProtocolError.badValue(field: "codec", value: UInt32(rawCodec))
            }
            let n = Int(try r.u8())
            _ = try r.u16()
            let width = try r.u32()
            let height = try r.u32()
            let fps = try r.u32()
            var sets: [[UInt8]] = []
            for _ in 0..<n { sets.append(try r.blob16()) }
            return .videoConfig(VideoConfig(codec: codec, width: width, height: height, fpsMilli: fps,
                                            parameterSets: sets))
        case MessageType.videoFrame:
            let id = try r.u32()
            let key = (try r.u8() & 1) != 0
            _ = try r.u8()
            _ = try r.u16()
            let cap = try r.u64()
            let enc = try r.u64()
            return .videoFrame(VideoFrame(frameID: id, keyframe: key, captureNs: cap, encodedNs: enc, data: r.rest()))
        case MessageType.videoFeedback:
            let id = try r.u32()
            let req = (try r.u8() & 1) != 0
            _ = try r.u8()
            _ = try r.u16()
            return .videoFeedback(VideoFeedback(frameID: id, requestKeyframe: req, receivedNs: try r.u64(),
                                                decodedNs: try r.u64(), displayedNs: try r.u64()))
        default:
            return .unknown(type: type, payload: payload)
        }
    }
}

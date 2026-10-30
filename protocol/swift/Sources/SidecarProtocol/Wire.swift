// Little-endian byte reader/writer used by the message codec.

public enum ProtocolError: Error, Equatable, Sendable {
    case frameTooLarge(Int)
    case emptyFrame
    case truncated(type: UInt8)
    case badString
    case badMagic
    case badValue(field: String, value: UInt32)
}

public struct ByteWriter: Sendable {
    public private(set) var bytes: [UInt8] = []

    public init() {}

    public mutating func u8(_ v: UInt8) { bytes.append(v) }
    public mutating func u16(_ v: UInt16) { withUnsafeBytes(of: v.littleEndian) { bytes.append(contentsOf: $0) } }
    public mutating func u32(_ v: UInt32) { withUnsafeBytes(of: v.littleEndian) { bytes.append(contentsOf: $0) } }
    public mutating func u64(_ v: UInt64) { withUnsafeBytes(of: v.littleEndian) { bytes.append(contentsOf: $0) } }
    public mutating func f32(_ v: Float) { u32(v.bitPattern) }
    public mutating func raw(_ b: [UInt8]) { bytes.append(contentsOf: b) }
    public mutating func bool(_ b: Bool) { u8(b ? 1 : 0) }

    /// u16 length + UTF-8 (truncated to 65535 bytes on a character boundary).
    public mutating func string(_ s: String) {
        var utf8 = Array(s.utf8)
        if utf8.count > Int(UInt16.max) {
            var end = Int(UInt16.max)
            // Back off continuation bytes (10xxxxxx) to a scalar boundary.
            while end > 0 && (utf8[end] & 0xC0) == 0x80 { end -= 1 }
            utf8 = Array(utf8[..<end])
        }
        u16(UInt16(utf8.count))
        raw(utf8)
    }

    public mutating func blob16(_ b: [UInt8]) {
        let n = min(b.count, Int(UInt16.max))
        u16(UInt16(n))
        raw(Array(b[..<n]))
    }
}

public struct ByteReader {
    let type: UInt8
    let data: [UInt8]
    var pos = 0

    init(type: UInt8, data: [UInt8]) {
        self.type = type
        self.data = data
    }

    var remaining: Int { data.count - pos }

    mutating func take(_ n: Int) throws -> ArraySlice<UInt8> {
        guard remaining >= n else { throw ProtocolError.truncated(type: type) }
        defer { pos += n }
        return data[pos..<pos + n]
    }

    mutating func u8() throws -> UInt8 { try take(1).first! }

    mutating func u16() throws -> UInt16 {
        let s = try take(2)
        return UInt16(s[s.startIndex]) | UInt16(s[s.startIndex + 1]) << 8
    }

    mutating func u32() throws -> UInt32 {
        let s = try take(4)
        var v: UInt32 = 0
        for (i, b) in s.enumerated() { v |= UInt32(b) << (8 * UInt32(i)) }
        return v
    }

    mutating func u64() throws -> UInt64 {
        let s = try take(8)
        var v: UInt64 = 0
        for (i, b) in s.enumerated() { v |= UInt64(b) << (8 * UInt64(i)) }
        return v
    }

    mutating func f32() throws -> Float { Float(bitPattern: try u32()) }

    mutating func array(_ n: Int) throws -> [UInt8] { Array(try take(n)) }

    mutating func string() throws -> String {
        let n = Int(try u16())
        let b = Array(try take(n))
        // String(validating:) needs iOS 18; decode and verify the round trip instead.
        let s = String(decoding: b, as: UTF8.self)
        guard Array(s.utf8) == b else { throw ProtocolError.badString }
        return s
    }

    mutating func blob16() throws -> [UInt8] {
        let n = Int(try u16())
        return Array(try take(n))
    }

    mutating func rest() -> [UInt8] {
        defer { pos = data.count }
        return Array(data[pos...])
    }
}

/// `length:u32le type:u8 payload`
public func encodeFrame(type: UInt8, payload: [UInt8]) -> [UInt8] {
    var w = ByteWriter()
    w.u32(UInt32(payload.count + 1))
    w.u8(type)
    w.raw(payload)
    return w.bytes
}

/// Incremental frame decoder: push bytes as they arrive, pop whole frames.
public struct FrameDecoder: Sendable {
    private var buffer: [UInt8] = []
    private var start = 0

    public init() {}

    public var buffered: Int { buffer.count - start }

    public mutating func push(_ bytes: [UInt8]) {
        if start > 0 && start == buffer.count {
            buffer.removeAll(keepingCapacity: true)
            start = 0
        } else if start > 64 * 1024 {
            buffer.removeFirst(start)
            start = 0
        }
        buffer.append(contentsOf: bytes)
    }

    public mutating func nextFrame() throws -> (type: UInt8, payload: [UInt8])? {
        guard buffered >= 4 else { return nil }
        let len = Int(buffer[start]) | Int(buffer[start + 1]) << 8 | Int(buffer[start + 2]) << 16
            | Int(buffer[start + 3]) << 24
        if len == 0 { throw ProtocolError.emptyFrame }
        if len > SidecarProtocol.maxFrameLength { throw ProtocolError.frameTooLarge(len) }
        guard buffered >= 4 + len else { return nil }
        let type = buffer[start + 4]
        let payload = Array(buffer[(start + 5)..<(start + 4 + len)])
        start += 4 + len
        return (type, payload)
    }
}

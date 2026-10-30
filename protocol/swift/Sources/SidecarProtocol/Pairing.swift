// Pairing math, identical to protocol/rust/src/pairing.rs.
import Crypto

public enum Pairing {
    static let commitDomain = Array("sidecarforlinux pair commit v1".utf8)
    static let sasDomain = Array("sidecarforlinux pair sas v1".utf8)

    /// SHA-256 of the DER certificate.
    public static func fingerprint(certificateDER: [UInt8]) -> [UInt8] {
        Array(SHA256.hash(data: certificateDER))
    }

    public static func commitment(ipadFingerprint: [UInt8], ipadNonce: [UInt8]) -> [UInt8] {
        var h = SHA256()
        h.update(data: commitDomain)
        h.update(data: ipadFingerprint)
        h.update(data: ipadNonce)
        return Array(h.finalize())
    }

    /// Six-digit code (0...999_999) both screens show.
    public static func sas(daemonFingerprint: [UInt8], ipadFingerprint: [UInt8], daemonNonce: [UInt8],
                           ipadNonce: [UInt8]) -> UInt32 {
        var h = SHA256()
        h.update(data: sasDomain)
        h.update(data: daemonFingerprint)
        h.update(data: ipadFingerprint)
        h.update(data: daemonNonce)
        h.update(data: ipadNonce)
        let d = Array(h.finalize())
        let v = UInt32(d[0]) << 24 | UInt32(d[1]) << 16 | UInt32(d[2]) << 8 | UInt32(d[3])
        return v % 1_000_000
    }

    /// "123 456"
    public static func formatSAS(_ code: UInt32) -> String {
        let hi = code / 1000
        let lo = code % 1000
        return pad3(hi) + " " + pad3(lo)
    }

    public static func hex(_ bytes: [UInt8]) -> String {
        let digits = Array("0123456789abcdef")
        var s = ""
        s.reserveCapacity(bytes.count * 2)
        for b in bytes {
            s.append(digits[Int(b >> 4)])
            s.append(digits[Int(b & 0xF)])
        }
        return s
    }

    /// "ab:cd:…" for display.
    public static func formatFingerprint(_ fp: [UInt8]) -> String {
        stride(from: 0, to: fp.count, by: 1).map { hex([fp[$0]]) }.joined(separator: ":")
    }

    public static func parseHex(_ s: String) -> [UInt8]? {
        let chars = Array(s.lowercased().filter { $0.isHexDigit })
        guard chars.count % 2 == 0 else { return nil }
        var out: [UInt8] = []
        var i = 0
        while i < chars.count {
            guard let b = UInt8(String(chars[i...i + 1]), radix: 16) else { return nil }
            out.append(b)
            i += 2
        }
        return out
    }

    private static func pad3(_ v: UInt32) -> String {
        let s = String(v)
        return String(repeating: "0", count: max(0, 3 - s.count)) + s
    }
}

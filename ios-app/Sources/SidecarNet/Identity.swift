import Crypto
import Foundation
import Security
import SidecarProtocol
import SwiftASN1
import X509

/// The iPad's long-lived TLS identity: a P-256 key and a self-signed
/// certificate, created on first launch and kept in the app's keychain.
/// Nothing here needs an entitlement a free Apple ID lacks.
public struct DeviceIdentity {
    public let identity: SecIdentity
    public let certificateDER: [UInt8]

    public var fingerprint: [UInt8] { Pairing.fingerprint(certificateDER: certificateDER) }

    /// Keychain/UserDefaults namespace; tests use their own.
    public struct Namespace: Sendable {
        public var keyTag: Data
        public var certLabel: String
        public var derDefaultsKey: String
        public var commonName: String

        public init(_ suffix: String, commonName: String) {
            keyTag = Data("io.github.nofy.sidecarforlinux.\(suffix).key".utf8)
            certLabel = "sidecarforlinux \(suffix)"
            derDefaultsKey = "\(suffix).certificateDER"
            self.commonName = commonName
        }

        public static let app = Namespace("identity", commonName: "Sidecar iPad")
    }

    public enum IdentityError: Error {
        case keychain(OSStatus, String)
        case notFound
    }

    /// Loads the identity, creating it on first use.
    public static func loadOrCreate(_ ns: Namespace = .app) throws -> DeviceIdentity {
        if let der = UserDefaults.standard.data(forKey: ns.derDefaultsKey), let id = try? find(der: [UInt8](der)) {
            return DeviceIdentity(identity: id, certificateDER: [UInt8](der))
        }
        try deleteAll(ns)
        return try create(ns)
    }

    /// Throws away the identity (every paired PC must pair again).
    public static func reset(_ ns: Namespace = .app) throws -> DeviceIdentity {
        try deleteAll(ns)
        return try create(ns)
    }

    public static func deleteAll(_ ns: Namespace) throws {
        SecItemDelete([kSecClass: kSecClassKey, kSecAttrApplicationTag: ns.keyTag] as CFDictionary)
        SecItemDelete([kSecClass: kSecClassCertificate, kSecAttrLabel: ns.certLabel] as CFDictionary)
        UserDefaults.standard.removeObject(forKey: ns.derDefaultsKey)
    }

    private static func create(_ ns: Namespace) throws -> DeviceIdentity {
        // 1. Key: P-256, generated inside the keychain (the private key never
        //    leaves it; swift-certificates signs through SecKey).
        var keyAttrs: [CFString: Any] = [
            kSecAttrIsPermanent: true,
            kSecAttrApplicationTag: ns.keyTag,
            kSecAttrLabel: ns.certLabel,
        ]
        #if os(iOS)
        // iOS: data-protection keychain, never synced or backed up off-device.
        // (On macOS this attribute would select the data-protection keychain,
        // which needs an entitlement unsigned test runners lack.)
        keyAttrs[kSecAttrAccessible] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        #endif
        let keyParams: [CFString: Any] = [
            kSecAttrKeyType: kSecAttrKeyTypeECSECPrimeRandom,
            kSecAttrKeySizeInBits: 256,
            kSecPrivateKeyAttrs: keyAttrs,
        ]
        var cfErr: Unmanaged<CFError>?
        guard let secKey = SecKeyCreateRandomKey(keyParams as CFDictionary, &cfErr) else {
            throw cfErr!.takeRetainedValue() as Error
        }

        // 2. Self-signed X.509 v3 certificate (digitalSignature is mandatory
        //    on iOS 18.4+, see docs/protocol.md § Security).
        let name = try DistinguishedName {
            CommonName(ns.commonName)
            OrganizationName("sidecarforlinux")
        }
        let now = Date()
        let privateKey = try Certificate.PrivateKey(secKey)
        let cert = try Certificate(
            version: .v3,
            serialNumber: Certificate.SerialNumber(),
            publicKey: privateKey.publicKey,
            notValidBefore: now.addingTimeInterval(-86_400),
            notValidAfter: now.addingTimeInterval(20 * 365 * 86_400),
            issuer: name,
            subject: name,
            signatureAlgorithm: .ecdsaWithSHA256,
            extensions: try Certificate.Extensions {
                Critical(BasicConstraints.notCertificateAuthority)
                Critical(KeyUsage(digitalSignature: true))
                try ExtendedKeyUsage([.serverAuth, .clientAuth])
                SubjectAlternativeNames([.dnsName("sidecar.invalid")])
            },
            issuerPrivateKey: privateKey
        )
        var serializer = DER.Serializer()
        try serializer.serialize(cert)
        let der = serializer.serializedBytes
        guard let secCert = SecCertificateCreateWithData(nil, Data(der) as CFData) else {
            throw IdentityError.keychain(errSecDecode, "certificate DER")
        }
        let status = SecItemAdd([
            kSecClass: kSecClassCertificate,
            kSecValueRef: secCert,
            kSecAttrLabel: ns.certLabel,
        ] as CFDictionary, nil)
        guard status == errSecSuccess || status == errSecDuplicateItem else {
            throw IdentityError.keychain(status, "add certificate")
        }
        UserDefaults.standard.set(Data(der), forKey: ns.derDefaultsKey)

        // 3. The keychain pairs key + certificate into a SecIdentity.
        return DeviceIdentity(identity: try find(der: der), certificateDER: der)
    }

    private static func find(der: [UInt8]) throws -> SecIdentity {
        var out: CFTypeRef?
        let status = SecItemCopyMatching([
            kSecClass: kSecClassIdentity,
            kSecReturnRef: true,
            kSecMatchLimit: kSecMatchLimitAll,
        ] as CFDictionary, &out)
        guard status == errSecSuccess, let list = out as? [SecIdentity] else { throw IdentityError.notFound }
        for id in list {
            var cert: SecCertificate?
            if SecIdentityCopyCertificate(id, &cert) == errSecSuccess, let cert,
               [UInt8](SecCertificateCopyData(cert) as Data) == der {
                return id
            }
        }
        throw IdentityError.notFound
    }
}

/// PCs this iPad has paired with (pinned SHA-256 certificate fingerprints).
public final class TrustStore: @unchecked Sendable {
    public struct Entry: Codable, Equatable, Sendable {
        public var name: String
        public var fingerprint: String
        public var pairedAt: Date
    }

    private let key: String
    private let lock = NSLock()
    public private(set) var entries: [Entry] = []

    public init(defaultsKey: String = "trustedDaemons") {
        key = defaultsKey
        if let data = UserDefaults.standard.data(forKey: key),
           let list = try? JSONDecoder().decode([Entry].self, from: data) {
            entries = list
        }
    }

    public func contains(_ fp: [UInt8]) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        let hex = Pairing.hex(fp)
        return entries.contains { $0.fingerprint == hex }
    }

    public func add(_ fp: [UInt8], name: String) {
        lock.lock()
        let hex = Pairing.hex(fp)
        entries.removeAll { $0.fingerprint == hex }
        entries.append(Entry(name: name, fingerprint: hex, pairedAt: Date()))
        save()
        lock.unlock()
    }

    public func removeAll() {
        lock.lock()
        entries.removeAll()
        save()
        lock.unlock()
    }

    private func save() {
        if let data = try? JSONEncoder().encode(entries) {
            UserDefaults.standard.set(data, forKey: key)
        }
    }
}

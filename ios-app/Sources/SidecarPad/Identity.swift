import Crypto
import Foundation
import Security
import SidecarProtocol
import SwiftASN1
import X509

/// The iPad's long-lived TLS identity: a P-256 key and a self-signed
/// certificate, created on first launch and kept in the app's keychain.
/// Nothing here needs an entitlement a free Apple ID lacks.
struct DeviceIdentity {
    let identity: SecIdentity
    let certificateDER: [UInt8]

    var fingerprint: [UInt8] { Pairing.fingerprint(certificateDER: certificateDER) }

    private static let keyTag = Data("io.github.nofy.sidecarforlinux.identity.key".utf8)
    private static let certLabel = "sidecarforlinux identity"
    private static let derDefaultsKey = "identity.certificateDER"

    enum IdentityError: Error {
        case keychain(OSStatus, String)
        case notFound
    }

    /// Loads the identity, creating it on first use.
    static func loadOrCreate() throws -> DeviceIdentity {
        if let der = UserDefaults.standard.data(forKey: derDefaultsKey), let id = try? find(der: [UInt8](der)) {
            return DeviceIdentity(identity: id, certificateDER: [UInt8](der))
        }
        try deleteAll()
        return try create()
    }

    /// Throws away the identity (every paired PC must pair again).
    static func reset() throws -> DeviceIdentity {
        try deleteAll()
        return try create()
    }

    private static func deleteAll() throws {
        SecItemDelete([kSecClass: kSecClassKey, kSecAttrApplicationTag: keyTag] as CFDictionary)
        SecItemDelete([kSecClass: kSecClassCertificate, kSecAttrLabel: certLabel] as CFDictionary)
        UserDefaults.standard.removeObject(forKey: derDefaultsKey)
    }

    private static func create() throws -> DeviceIdentity {
        // 1. Key: generated in software so swift-certificates can sign with it,
        //    then stored in the keychain.
        let key = P256.Signing.PrivateKey()
        let attrs: [CFString: Any] = [
            kSecAttrKeyType: kSecAttrKeyTypeECSECPrimeRandom,
            kSecAttrKeyClass: kSecAttrKeyClassPrivate,
            kSecAttrKeySizeInBits: 256,
        ]
        var cfErr: Unmanaged<CFError>?
        guard let secKey = SecKeyCreateWithData(key.x963Representation as CFData, attrs as CFDictionary, &cfErr) else {
            throw cfErr!.takeRetainedValue() as Error
        }
        var status = SecItemAdd([
            kSecClass: kSecClassKey,
            kSecValueRef: secKey,
            kSecAttrApplicationTag: keyTag,
            kSecAttrAccessible: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
        ] as CFDictionary, nil)
        guard status == errSecSuccess || status == errSecDuplicateItem else {
            throw IdentityError.keychain(status, "add key")
        }

        // 2. Self-signed X.509 v3 certificate (digitalSignature is mandatory
        //    on iOS 18.4+, see docs/protocol.md § Security).
        let name = try DistinguishedName {
            CommonName("Sidecar iPad")
            OrganizationName("sidecarforlinux")
        }
        let now = Date()
        let privateKey = Certificate.PrivateKey(key)
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
        status = SecItemAdd([
            kSecClass: kSecClassCertificate,
            kSecValueRef: secCert,
            kSecAttrLabel: certLabel,
        ] as CFDictionary, nil)
        guard status == errSecSuccess || status == errSecDuplicateItem else {
            throw IdentityError.keychain(status, "add certificate")
        }
        UserDefaults.standard.set(Data(der), forKey: derDefaultsKey)

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
final class TrustStore {
    struct Entry: Codable, Equatable {
        var name: String
        var fingerprint: String
        var pairedAt: Date
    }

    private let key = "trustedDaemons"
    private let lock = NSLock()
    private(set) var entries: [Entry] = []

    init() {
        if let data = UserDefaults.standard.data(forKey: key),
           let list = try? JSONDecoder().decode([Entry].self, from: data) {
            entries = list
        }
    }

    func contains(_ fp: [UInt8]) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        let hex = Pairing.hex(fp)
        return entries.contains { $0.fingerprint == hex }
    }

    func add(_ fp: [UInt8], name: String) {
        lock.lock()
        let hex = Pairing.hex(fp)
        entries.removeAll { $0.fingerprint == hex }
        entries.append(Entry(name: name, fingerprint: hex, pairedAt: Date()))
        save()
        lock.unlock()
    }

    func removeAll() {
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

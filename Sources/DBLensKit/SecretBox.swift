import CryptoKit
import Darwin
import Foundation

/// Encrypts small secrets (the saved connection URLs) with AES-GCM, using a key
/// bound to this Mac's Secure Enclave. The key file holds the Secure Enclave
/// key's opaque blob, which only this Mac's chip can use, plus a public key;
/// the AES key is derived from both and never written down. Copied to another
/// Mac, neither file is any use. No Keychain, so no password prompts.
/// Macs without a Secure Enclave fall back to a random key in the key file
/// (encrypted at rest, but the key sits beside the data).
public struct SecretBox: Sendable {
    /// Part of the key derivation. It keeps the app's first name, PgLens: changing
    /// it would make every saved connection unreadable.
    private static let salt = Data("PgLens".utf8)

    private let key: SymmetricKey
    /// True when the key lives in the Secure Enclave.
    public let hardwareBound: Bool

    private struct KeyFile: Codable {
        var kind: String          // "secure-enclave" or "software"
        var enclaveKey: Data?     // SecureEnclave.P256.KeyAgreement.PrivateKey.dataRepresentation
        var peerPublicKey: Data?  // P256 public key, raw representation
        var softwareKey: Data?    // 32 random bytes
    }

    public enum Failure: Error, LocalizedError {
        case unreadable
        public var errorDescription: String? {
            String(localized: "Couldn't open the saved connections: this Mac's key does not match. Paste the URLs again.")
        }
    }

    /// Loads the key from `keyFile`. Makes a new one there (permissions 0600)
    /// only when the file does not exist and `create` is true: a key that can't
    /// be read is never replaced, since that would lose everything it encrypted.
    public init(keyFile: URL, create: Bool = true) throws {
        if FileManager.default.fileExists(atPath: keyFile.path) {
            do {
                let file = try JSONDecoder().decode(KeyFile.self, from: Data(contentsOf: keyFile))
                (key, hardwareBound) = try Self.key(from: file)
            } catch {
                throw Failure.unreadable
            }
            return
        }
        guard create else { throw Failure.unreadable }
        let file = try Self.newKeyFile()
        try writePrivately(JSONEncoder().encode(file), to: keyFile)
        (key, hardwareBound) = try Self.key(from: file)
    }

    public func seal(_ plain: Data) throws -> Data {
        guard let combined = try AES.GCM.seal(plain, using: key).combined else { throw Failure.unreadable }
        return combined
    }

    public func open(_ sealed: Data) throws -> Data {
        do {
            return try AES.GCM.open(AES.GCM.SealedBox(combined: sealed), using: key)
        } catch {
            throw Failure.unreadable
        }
    }

    private static func newKeyFile() throws -> KeyFile {
        if SecureEnclave.isAvailable, let enclave = try? SecureEnclave.P256.KeyAgreement.PrivateKey() {
            // The peer's private half is thrown away: the secret needs the enclave key.
            let peer = P256.KeyAgreement.PrivateKey()
            _ = try enclave.sharedSecretFromKeyAgreement(with: peer.publicKey)
            return KeyFile(kind: "secure-enclave", enclaveKey: enclave.dataRepresentation,
                           peerPublicKey: peer.publicKey.rawRepresentation)
        }
        return KeyFile(kind: "software", softwareKey: SymmetricKey(size: .bits256).withUnsafeBytes { Data($0) })
    }

    private static func key(from file: KeyFile) throws -> (SymmetricKey, Bool) {
        switch file.kind {
        case "secure-enclave":
            guard let blob = file.enclaveKey, let peer = file.peerPublicKey else { throw Failure.unreadable }
            let enclave = try SecureEnclave.P256.KeyAgreement.PrivateKey(dataRepresentation: blob)
            let secret = try enclave.sharedSecretFromKeyAgreement(with: P256.KeyAgreement.PublicKey(rawRepresentation: peer))
            let key = secret.hkdfDerivedSymmetricKey(using: SHA256.self, salt: Self.salt,
                                                     sharedInfo: Data("connections v1".utf8), outputByteCount: 32)
            return (key, true)
        default:
            guard let raw = file.softwareKey, raw.count == 32 else { throw Failure.unreadable }
            return (SymmetricKey(data: raw), false)
        }
    }
}

/// Writes a file that only the current user can read, from the first byte:
/// the temporary file `.atomic` makes is created under a 077 umask, so it is
/// never readable by others, even for a moment.
public func writePrivately(_ data: Data, to url: URL) throws {
    let previous = umask(0o077)
    defer { umask(previous) }
    try data.write(to: url, options: .atomic)
    try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: url.path)
}

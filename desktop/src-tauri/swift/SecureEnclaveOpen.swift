// The one piece of the dbLens for Mac importer that needs CryptoKit: opening
// connections.enc when its key is bound to this Mac's Secure Enclave
// (kind "secure-enclave" in connections.key). Same derivation as
// Sources/DBLensKit/SecretBox.swift; never change one without the other.
//
// Built by build.rs (macOS targets only) into a static library and called
// from src/import_swift.rs. Plain C ABI, no allocation crosses it: AES-GCM's
// plaintext is exactly 28 bytes shorter than the sealed box (12-byte nonce,
// 16-byte tag), so Rust passes a buffer of that size and Swift fills it.

import CryptoKit
import Foundation

/// Status codes; keep in step with `enclave_status` in src/import_swift.rs.
private let opened: Int32 = 0
private let badInput: Int32 = 1
private let noEnclave: Int32 = 2
private let keyUnusable: Int32 = 3
private let cantOpen: Int32 = 4
private let wrongLength: Int32 = 5

private func data(_ pointer: UnsafePointer<UInt8>?, _ count: Int) -> Data? {
    guard let pointer = pointer, count > 0 else { return nil }
    return Data(bytes: pointer, count: count)
}

@_cdecl("dblens_secure_enclave_open")
public func dblensSecureEnclaveOpen(
    _ enclaveKey: UnsafePointer<UInt8>?, _ enclaveKeyCount: Int,
    _ peerPublicKey: UnsafePointer<UInt8>?, _ peerPublicKeyCount: Int,
    _ sealed: UnsafePointer<UInt8>?, _ sealedCount: Int,
    _ out: UnsafeMutablePointer<UInt8>?, _ outCount: Int
) -> Int32 {
    guard let blob = data(enclaveKey, enclaveKeyCount),
          let peer = data(peerPublicKey, peerPublicKeyCount),
          let box = data(sealed, sealedCount),
          let out = out
    else { return badInput }
    guard SecureEnclave.isAvailable else { return noEnclave }

    let key: SymmetricKey
    do {
        let enclave = try SecureEnclave.P256.KeyAgreement.PrivateKey(dataRepresentation: blob)
        let secret = try enclave.sharedSecretFromKeyAgreement(
            with: P256.KeyAgreement.PublicKey(rawRepresentation: peer))
        key = secret.hkdfDerivedSymmetricKey(
            using: SHA256.self, salt: Data("PgLens".utf8),
            sharedInfo: Data("connections v1".utf8), outputByteCount: 32)
    } catch {
        return keyUnusable
    }

    let plain: Data
    do {
        plain = try AES.GCM.open(AES.GCM.SealedBox(combined: box), using: key)
    } catch {
        return cantOpen
    }
    guard plain.count == outCount else { return wrongLength }
    plain.copyBytes(to: out, count: plain.count)
    return opened
}

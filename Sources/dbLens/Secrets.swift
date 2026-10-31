import Foundation
import DBLensKit
import Security

/// Connection URLs, which hold passwords, encrypted with AES-GCM under a key
/// bound to this Mac's Secure Enclave (SecretBox), in files only this Mac user
/// can read (permissions 0600):
///   ~/Library/Application Support/dbLens/connections.enc   the URLs, encrypted
///   ~/Library/Application Support/dbLens/connections.key   the Secure Enclave key blob
/// Not the Keychain: it asked for the Mac password after every rebuild. Older
/// versions' Keychain items and plain connections.json move here once.
/// Test runs (APP_STORE in /tmp) keep them in memory.
enum Secrets {
    private static var cache: [UUID: String]?
    /// Why the saved URLs could not be read, for the connection error.
    private(set) static var loadError: String?

    private static var folder: URL { Store.url.deletingLastPathComponent() }
    static var fileURL: URL { folder.appending(path: "connections.enc") }
    private static var keyURL: URL { folder.appending(path: "connections.key") }
    /// The previous, unencrypted file.
    private static var plainURL: URL { folder.appending(path: "connections.json") }

    static func url(for id: UUID) -> String? { load()[id] }

    static func setURL(_ url: String, for id: UUID) throws {
        var all = load()
        all[id] = url
        try write(all)
    }

    static func removeURL(for id: UUID) {
        var all = load()
        all[id] = nil
        try? write(all)
    }

    private static func load() -> [UUID: String] {
        if let cache { return cache }
        guard !Store.isOverridden else {
            cache = [:]
            return [:]
        }
        // The folder holds the store, the key, and the encrypted URLs: this user only.
        try? FileManager.default.setAttributes([.posixPermissions: 0o700], ofItemAtPath: folder.path)
        if let sealed = try? Data(contentsOf: fileURL) {
            do {
                cache = decode(try box(create: false).open(sealed)) ?? [:]
                // A plain copy left by an older version must not outlive the encrypted one.
                try? FileManager.default.removeItem(at: plainURL)
            } catch {
                loadError = error.localizedDescription
                cache = [:]
            }
        } else if let plain = try? Data(contentsOf: plainURL), let all = decode(plain) {
            // Encrypt what the previous version saved in the clear, then remove it.
            // (A damaged plain file is left alone rather than replaced by nothing.)
            if (try? write(all)) != nil { try? FileManager.default.removeItem(at: plainURL) }
            cache = all
        } else {
            cache = moveFromKeychain()
        }
        return cache ?? [:]
    }

    /// The key is made only when there are no encrypted URLs yet: an existing
    /// file encrypted with a key that is gone stays as it is.
    private static func box(create: Bool) throws -> SecretBox {
        try SecretBox(keyFile: keyURL, create: create && !FileManager.default.fileExists(atPath: fileURL.path))
    }

    private static func write(_ all: [UUID: String]) throws {
        cache = all
        guard !Store.isOverridden else { return }
        if loadError != nil { setAsideUnreadable() }
        let data = try JSONEncoder().encode(Dictionary(uniqueKeysWithValues: all.map { ($0.key.uuidString, $0.value) }))
        do {
            try writePrivately(box(create: true).seal(data), to: fileURL)
        } catch {
            throw SecretsError(path: fileURL.path, underlying: error)
        }
    }

    /// When the saved URLs can't be opened (the key is gone or damaged), the
    /// next save starts over, keeping the old files beside it, renamed.
    private static func setAsideUnreadable() {
        let stamp = Date.now.formatted(.iso8601.year().month().day().time(includingFractionalSeconds: false))
            .replacingOccurrences(of: ":", with: "")
        for url in [fileURL, keyURL] where FileManager.default.fileExists(atPath: url.path) {
            try? FileManager.default.moveItem(at: url, to: url.appendingPathExtension("unreadable-\(stamp)"))
        }
        loadError = nil
    }

    /// nil when the data is not the expected JSON.
    private static func decode(_ data: Data) -> [UUID: String]? {
        guard let decoded = try? JSONDecoder().decode([String: String].self, from: data) else { return nil }
        return Dictionary(uniqueKeysWithValues: decoded.compactMap { key, url in UUID(uuidString: key).map { ($0, url) } })
    }

    // MARK: Moving out of the Keychain

    private static let keychainService = "com.miguelplaza.dblens"
    private static let keychainAccount = "connections"
    /// The first version kept one item per connection under this service.
    private static let legacyService = "com.miguelplaza.dblens.connection"

    /// Reads what earlier versions kept in the Keychain, writes it to the file,
    /// and deletes the Keychain items. macOS may ask for the password this one time.
    private static func moveFromKeychain() -> [UUID: String] {
        var all: [UUID: String] = [:]
        if let data = keychainData(service: keychainService, account: keychainAccount) {
            all = decode(data) ?? [:]
        }
        for item in legacyItems() where all[item.0] == nil { all[item.0] = item.1 }
        guard (try? write(all)) != nil else { return all }
        SecItemDelete([kSecClass as String: kSecClassGenericPassword,
                       kSecAttrService as String: keychainService] as CFDictionary)
        SecItemDelete([kSecClass as String: kSecClassGenericPassword,
                       kSecAttrService as String: legacyService] as CFDictionary)
        return all
    }

    private static func keychainData(service: String, account: String) -> Data? {
        var result: AnyObject?
        let status = SecItemCopyMatching([
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne,
        ] as CFDictionary, &result)
        return status == errSecSuccess ? result as? Data : nil
    }

    /// The first version's items: one per connection, the account is the connection id.
    private static func legacyItems() -> [(UUID, String)] {
        var result: AnyObject?
        let status = SecItemCopyMatching([
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: legacyService,
            kSecReturnAttributes as String: true,
            kSecMatchLimit as String: kSecMatchLimitAll,
        ] as CFDictionary, &result)
        guard status == errSecSuccess, let items = result as? [[String: Any]] else { return [] }
        return items.compactMap { item in
            guard let account = item[kSecAttrAccount as String] as? String, let id = UUID(uuidString: account),
                  let data = keychainData(service: legacyService, account: account),
                  let url = String(data: data, encoding: .utf8) else { return nil }
            return (id, url)
        }
    }
}

struct SecretsError: LocalizedError {
    let path: String
    let underlying: Error
    var errorDescription: String? {
        String(localized: "Couldn't save the URL at \(path): \(underlying.localizedDescription)")
    }
}

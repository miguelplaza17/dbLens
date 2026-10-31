import Foundation
import DBLensKit

/// Connection URLs, which hold passwords, encrypted with AES-GCM under a key
/// bound to this Mac's Secure Enclave (SecretBox), in files only this Mac user
/// can read (permissions 0600):
///   ~/Library/Application Support/dbLens/connections.enc   the URLs, encrypted
///   ~/Library/Application Support/dbLens/connections.key   the Secure Enclave key blob
/// Not the Keychain: it asked for the Mac password after every rebuild. Older
/// versions' plain connections.json moves here once.
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

    /// Reads the file again on the next use: another process (the MCP server)
    /// may have added or removed a connection.
    static func reload() {
        if !Store.isOverridden { cache = nil }
    }

    static func setURL(_ url: String, for id: UUID) throws {
        reload()  // never write back a stale copy over another process's change
        var all = load()
        all[id] = url
        try write(all)
    }

    static func removeURL(for id: UUID) {
        reload()
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
            cache = [:]
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

}

struct SecretsError: LocalizedError {
    let path: String
    let underlying: Error
    var errorDescription: String? {
        String(localized: "Couldn't save the URL at \(path): \(underlying.localizedDescription)")
    }
}

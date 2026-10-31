import Foundation

/// The official command-line client for a connection (psql, mysql, sqlite3,
/// redis-cli, mongosh), ready to start in a terminal, for what the app doesn't
/// do: scripts, JavaScript, \commands. Passwords go in the environment when the
/// client reads them from there, never in the arguments.
public struct ClientCommand: Sendable, Equatable {
    /// Executable names to look for, in order of preference.
    public let tools: [String]
    public let arguments: [String]
    public let environment: [String: String]
    /// How to install the client with Homebrew.
    public let install: String

    /// The command for a saved connection's text. `tunnelPort` is the local end
    /// of the app's SSH tunnel, when the connection goes through one.
    public static func make(for text: String, tunnelPort: Int? = nil) throws -> ClientCommand {
        switch Engine.detect(text) {
        case .postgres:
            let url = try ConnectionURL(text)
            return ClientCommand(tools: ["psql"], arguments: [url.conninfoWithoutPassword(localPort: tunnelPort)],
                                 environment: url.password.map { ["PGPASSWORD": $0] } ?? [:],
                                 install: "brew install libpq")
        case .mysql:
            let url = try MySQLURL(text)
            var arguments = ["--host=\(tunnelPort == nil ? url.host : "127.0.0.1")",
                             "--port=\(tunnelPort ?? url.port ?? 3306)"]
            if tunnelPort == nil, let socket = url.socket { arguments = ["--socket=\(socket)"] } else { arguments.append("--protocol=TCP") }
            if let user = url.user { arguments.append("--user=\(user)") }
            if let mode = url.sslMode { arguments.append("--ssl-mode=\(mode)") }
            if let database = url.database { arguments.append(database) }
            return ClientCommand(tools: ["mysql", "mariadb"], arguments: arguments,
                                 environment: url.password.map { ["MYSQL_PWD": $0] } ?? [:],
                                 install: "brew install mysql-client")
        case .sqlite:
            return ClientCommand(tools: ["sqlite3"], arguments: [try SQLiteLocation(text).path], environment: [:],
                                 install: "brew install sqlite")
        case .redis:
            let url = try RedisURL(text)
            var arguments = ["-h", tunnelPort == nil ? url.host : "127.0.0.1", "-p", "\(tunnelPort ?? url.port)", "-n", "\(url.database)"]
            if let user = url.user { arguments += ["--user", user] }
            if url.tls { arguments.append("--tls") }
            if url.tls, !url.verifyTLS { arguments.append("--insecure") }
            return ClientCommand(tools: ["redis-cli", "valkey-cli"], arguments: arguments,
                                 environment: url.password.map { ["REDISCLI_AUTH": $0] } ?? [:],
                                 install: "brew install redis")
        case .mongodb:
            // mongosh takes no password from the environment: it goes in the URL.
            let url = try MongoURL(text)
            var target = url.raw
            if let tunnelPort, let parts = URLParts(url.raw) {
                var auth = ""
                if let user = parts.user {
                    auth = Self.encode(user) + (parts.password.map { ":" + Self.encode($0) } ?? "") + "@"
                }
                var query = parts.query.map { "\(Self.encode($0.key))=\(Self.encode($0.value))" }.sorted()
                query.append("directConnection=true")
                target = "mongodb://\(auth)127.0.0.1:\(tunnelPort)/\(parts.path)?" + query.joined(separator: "&")
            }
            return ClientCommand(tools: ["mongosh"], arguments: [target], environment: [:], install: "brew install mongosh")
        case nil:
            throw DatabaseError(message: String(localized: "This doesn't look like a database URL."))
        }
    }

    /// Folders where Homebrew and friends put these clients; a Mac app doesn't get the shell's PATH.
    public static let searchPath = [
        "/opt/homebrew/bin", "/usr/local/bin", "/opt/homebrew/opt/libpq/bin", "/opt/homebrew/opt/postgresql@18/bin",
        "/opt/homebrew/opt/postgresql@17/bin", "/opt/homebrew/opt/postgresql@16/bin", "/opt/homebrew/opt/mysql-client/bin",
        "/opt/homebrew/opt/sqlite/bin", "/Applications/Postgres.app/Contents/Versions/latest/bin", "/usr/bin",
    ]

    /// The first installed tool, or nil.
    public func resolve(in folders: [String] = searchPath) -> String? {
        for tool in tools {
            for folder in folders {
                let path = "\(folder)/\(tool)"
                if FileManager.default.isExecutableFile(atPath: path) { return path }
            }
        }
        return nil
    }

    /// A zsh script that starts the client and removes itself first, so the
    /// password in it lives only until the client starts.
    public func script(executable: String) -> String {
        func quote(_ value: String) -> String { "'" + value.replacingOccurrences(of: "'", with: "'\\''") + "'" }
        var lines = ["#!/bin/zsh", "rm -f -- \"$0\""]
        for (key, value) in environment.sorted(by: { $0.key < $1.key }) { lines.append("export \(key)=\(quote(value))") }
        lines.append("clear")
        lines.append((["exec", quote(executable)] + arguments.map(quote)).joined(separator: " "))
        return lines.joined(separator: "\n") + "\n"
    }

    private static func encode(_ text: String) -> String {
        text.addingPercentEncoding(withAllowedCharacters: CharacterSet(charactersIn:
            "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~")) ?? text
    }
}

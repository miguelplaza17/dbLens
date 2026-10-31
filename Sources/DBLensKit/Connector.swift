import Foundation

/// Connects to whatever database a pasted string points at: the engine is
/// read from the text, never chosen by hand.
public enum Connector {
    /// What the connection sheet shows under the URL, and the default name.
    public struct Summary: Sendable, Hashable {
        public let engine: Engine
        public let database: String
        public let host: String?
        public let port: Int?
        public let user: String?
        public let tls: String?
        public let suggestedName: String
    }

    /// The text as it will be saved: without an .env name or quotes around it.
    public static func clean(_ text: String) -> String { ConnectionURL.clean(text) }

    public static func summary(_ text: String) -> Summary? {
        switch Engine.detect(text) {
        case .postgres:
            guard let url = try? ConnectionURL(text) else { return nil }
            return Summary(engine: .postgres, database: url.database, host: url.host, port: url.port,
                           user: url.user, tls: url.sslmode, suggestedName: url.suggestedName)
        case .mysql:
            guard let url = try? MySQLURL(text) else { return nil }
            return Summary(engine: .mysql, database: url.database ?? "", host: url.host, port: url.port,
                           user: url.user, tls: url.sslMode, suggestedName: url.suggestedName)
        case .mongodb:
            guard let url = try? MongoURL(text) else { return nil }
            return Summary(engine: .mongodb, database: url.database ?? "", host: url.hosts.first.map { "\($0)" },
                           port: nil, user: url.user, tls: nil, suggestedName: url.suggestedName)
        case .redis:
            guard let url = try? RedisURL(text) else { return nil }
            return Summary(engine: .redis, database: "db \(url.database)", host: url.host, port: url.port,
                           user: url.user, tls: url.tls ? "TLS" : nil, suggestedName: url.suggestedName)
        case .sqlite:
            guard let location = try? SQLiteLocation(text) else { return nil }
            return Summary(engine: .sqlite, database: location.path, host: nil, port: nil, user: nil, tls: nil,
                           suggestedName: location.suggestedName)
        case nil:
            return nil
        }
    }

    public static func connect(_ text: String, ssh: SSHTunnel.Config?) async throws -> any DatabaseSession {
        guard let engine = Engine.detect(text) else {
            throw DatabaseError(message: String(localized: "This doesn't look like a database URL."))
        }
        switch engine {
        case .postgres:
            let url = try ConnectionURL(text)
            if let ssh { return try await PostgresSession.connect(url, via: ssh) }
            return try await PostgresSession.connect(url)
        case .mysql:
            return try await MySQLSession.connect(try MySQLURL(text), ssh: ssh)
        case .redis:
            return try await RedisSession.connect(try RedisURL(text), ssh: ssh)
        case .mongodb:
            return try await MongoSession.connect(try MongoURL(text), ssh: ssh)
        case .sqlite:
            guard ssh == nil else {
                throw DatabaseError(message: String(localized: "A SQLite file is on this Mac; SSH doesn't apply."))
            }
            return try await SQLiteSession.open(try SQLiteLocation(text))
        }
    }
}

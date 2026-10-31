import Foundation

/// The kinds of database dbLens knows, told apart by what was pasted, so
/// nobody has to pick one before connecting.
public enum Engine: String, CaseIterable, Sendable, Codable {
    case postgres, mysql, sqlite, redis, mongodb

    public var name: String {
        switch self {
        case .postgres: "PostgreSQL"
        case .mysql: "MySQL"
        case .sqlite: "SQLite"
        case .redis: "Redis"
        case .mongodb: "MongoDB"
        }
    }

    /// The URL scheme the form writes.
    public var scheme: String {
        switch self {
        case .postgres: "postgresql"
        case .mysql: "mysql"
        case .sqlite: "sqlite"
        case .redis: "redis"
        case .mongodb: "mongodb"
        }
    }

    /// nil for SQLite, which is a file.
    public var defaultPort: Int? {
        switch self {
        case .postgres: 5432
        case .mysql: 3306
        case .sqlite: nil
        case .redis: 6379
        case .mongodb: 27017
        }
    }

    /// Engines dbLens can connect to in this version.
    public var isAvailable: Bool { true }

    /// The engine a pasted connection string is for, or nil when it isn't one.
    ///   postgres://  postgresql://  jdbc:postgresql://  postgresql+driver://  host=… dbname=…
    ///   mysql://  mariadb://  mysqlx://  jdbc:mysql://  jdbc:mariadb://  mysql+driver://
    ///   sqlite:…  file:…  a path ending in .db, .sqlite, .sqlite3, .db3
    ///   redis://  rediss://  redis+sentinel://
    ///   mongodb://  mongodb+srv://
    public static func detect(_ text: String) -> Engine? {
        let value = ConnectionURL.clean(text)
        guard !value.isEmpty else { return nil }
        let lower = value.lowercased()
        let rules: [(String, Engine)] = [
            ("^(jdbc:)?postgres(ql)?(\\+[a-z0-9_]+)?://", .postgres),
            ("^(jdbc:)?(mysql|mariadb|mysqlx)(\\+[a-z0-9_]+)?://", .mysql),
            ("^(sqlite3?|file):", .sqlite),
            ("^rediss?(\\+[a-z]+)?://", .redis),
            ("^mongodb(\\+srv)?://", .mongodb),
        ]
        for (pattern, engine) in rules where lower.range(of: pattern, options: .regularExpression) != nil {
            return engine
        }
        if lower.range(of: "\\.(db|db3|sqlite|sqlite3)$", options: .regularExpression) != nil,
           lower.hasPrefix("/") || lower.hasPrefix("~") || lower.hasPrefix(".") {
            return .sqlite
        }
        // A SQLite path in an .env line (DB_PATH=/x/app.db).
        if (try? SQLiteLocation(text)) != nil { return .sqlite }
        // libpq's key=value form: host=… dbname=… user=…
        if (try? ConnectionURL(value)) != nil { return .postgres }
        return nil
    }
}

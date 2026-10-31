import Foundation

/// A Postgres connection, as pasted: any of the usual ways people write it.
///   postgres://user:pass@host:5432/db?sslmode=require   (also postgresql://)
///   postgresql://user@host1:5432,host2:5433/db           (several hosts)
///   postgresql:///db?host=/var/run/postgresql            (local socket)
///   jdbc:postgresql://host/db?user=ana&password=…&ssl=true
///   postgresql+psycopg2://…  postgres+asyncpg://…          (SQLAlchemy)
///   DATABASE_URL="postgres://…?schema=public"            (an .env line, Prisma)
///   host=db.example.com port=5432 dbname=loja user=ana   (libpq key=value)
/// Everything becomes libpq's own key=value settings. Driver-only options are
/// translated (ssl=true, sslmode=no-verify, schema=…) or dropped, because
/// libpq refuses any setting it doesn't know.
public struct ConnectionURL: Sendable, Hashable {
    /// What was pasted, without an .env name or quotes around it.
    public let raw: String
    /// libpq settings: host, port, dbname, user, password, sslmode, …
    public let settings: [String: String]

    public enum Problem: Error, Equatable, LocalizedError {
        case empty, notPostgres

        public var errorDescription: String? {
            switch self {
            case .empty: String(localized: "Paste the database URL.")
            case .notPostgres: String(localized: "Unrecognized format. Use a postgres://… URL (or postgresql://, jdbc:postgresql://) or the host=… dbname=… user=… format")
            }
        }
    }

    public init(_ text: String) throws(Problem) {
        let cleaned = Self.clean(text)
        guard !cleaned.isEmpty else { throw .empty }
        let parsed: [String: String]?
        if let url = Self.urlForm(cleaned) {
            parsed = Self.parseURL(url)
        } else if cleaned.contains("=") {
            parsed = Self.parseKeyValue(cleaned)
        } else {
            parsed = nil
        }
        guard let parsed, !parsed.isEmpty else { throw .notPostgres }
        let settings = Self.normalize(parsed)
        // key=value text with no setting libpq knows is not a connection.
        guard !settings.isEmpty else { throw .notPostgres }
        raw = cleaned
        self.settings = settings
    }

    // MARK: Reading the parts

    /// All hosts, comma separated, as libpq takes them. Empty means the local socket.
    public var hosts: String { settings["host"] ?? "" }
    /// The first host, for names and the form.
    public var host: String {
        let first = hosts.split(separator: ",").first.map(String.init) ?? ""
        if first.isEmpty || first.hasPrefix("/") { return "localhost" }
        return first
    }
    public var port: Int? { settings["port"].flatMap { $0.split(separator: ",").first }.flatMap { Int($0) } }
    public var user: String? { settings["user"] }
    public var password: String? { settings["password"] }
    public var sslmode: String? { settings["sslmode"] }
    /// libpq's default database is the user's name.
    public var database: String { settings["dbname"] ?? user ?? "postgres" }

    /// "loja @ db.example.com" for the sidebar.
    public var suggestedName: String { "\(database) @ \(host)" }

    /// What stands in for a saved password on screen.
    public static let mask = "•••"

    /// Settings whose values are secret.
    private static let secretKeys = ["password", "sslpassword"]

    /// Where a secret can sit in what was pasted.
    private enum Place: CaseIterable {
        case userinfo, query, keyValue

        /// A pattern whose first group is the text before the value.
        func pattern(_ key: String, value: String) -> String {
            switch self {
            // user:password@host; the password may hold "@", so up to the last one.
            case .userinfo: "(://[^:/?#@]*:)\(value)(?=@[^@/?#]*(?:[/?#]|$))"
            case .query: "(?i)([?&]\(key)=)\(value)"
            case .keyValue: "(?i)((?:^|\\s)\(key)\\s*=\\s*)\(value)"
            }
        }

        /// What a value looks like there.
        var value: String {
            switch self {
            case .userinfo: "[^/?#]*"
            case .query: "[^&#]*"
            case .keyValue: "(?:'(?:[^'\\\\]|\\\\.)*'|\\S+)"
            }
        }

        static func places(for key: String) -> [Place] { key == "password" ? allCases : [.query, .keyValue] }
    }

    /// What was pasted, with passwords hidden, safe to show on screen.
    public var redacted: String { Self.redact(raw) }

    /// Any connection text (any engine) with its passwords hidden.
    public static func redact(_ raw: String) -> String {
        var text = raw
        for key in Self.secretKeys {
            for place in Place.places(for: key) {
                text = text.replacingOccurrences(of: place.pattern(key, value: place.value),
                                                 with: "$1" + mask, options: .regularExpression)
            }
        }
        return text
    }

    /// Puts saved secrets back where the text still shows the mask, so editing
    /// a saved connection never needs a password on screen. A value typed over
    /// the mask is kept as typed.
    public static func unmask(_ text: String, original: ConnectionURL) -> String {
        unmask(text, secrets: original.settings)
    }

    /// The same for any engine, from the saved text.
    public static func unmask(_ text: String, originalText: String) -> String {
        unmask(text, secrets: secrets(in: originalText))
    }

    /// The password and sslpassword in any connection text.
    public static func secrets(in text: String) -> [String: String] {
        if let parts = URLParts(text) {
            var found: [String: String] = [:]
            found["password"] = parts.password
            found["sslpassword"] = parts.query.first { $0.key.lowercased() == "sslpassword" }?.value
            return found
        }
        return (try? ConnectionURL(text))?.settings ?? [:]
    }

    private static func unmask(_ text: String, secrets: [String: String]) -> String {
        guard text.contains(mask) else { return text }
        let masked = NSRegularExpression.escapedPattern(for: mask)
        var result = text
        for key in secretKeys {
            guard let secret = secrets[key] else { continue }
            for place in Place.places(for: key) {
                let value = place == .keyValue
                    ? "'" + secret.replacingOccurrences(of: "\\", with: "\\\\").replacingOccurrences(of: "'", with: "\\'") + "'"
                    : encode(secret)
                result = result.replacingOccurrences(of: place.pattern(key, value: masked),
                                                     with: "$1" + NSRegularExpression.escapedTemplate(for: value),
                                                     options: .regularExpression)
            }
        }
        return result
    }

    /// What libpq gets: key=value settings, plus a connect timeout so a wrong
    /// host fails in seconds instead of hanging.
    public var forLibpq: String { libpq(settings) }

    /// Settings for the psql command line: no password (it goes in PGPASSWORD) and,
    /// through a tunnel, the local port.
    public func conninfoWithoutPassword(localPort: Int? = nil) -> String {
        var all = settings
        all["password"] = nil
        all["sslpassword"] = nil
        if let localPort {
            all["host"] = "127.0.0.1"
            all["port"] = "\(localPort)"
            all["hostaddr"] = nil
        }
        return Self.order(all).map { "\($0.key)='\(Self.escape($0.value))'" }.joined(separator: " ")
    }

    /// The same settings aimed at a local port, for a connection through an SSH tunnel.
    public func forLibpq(throughLocalPort port: Int) -> String {
        var all = settings
        all["host"] = "127.0.0.1"
        all["port"] = "\(port)"
        all["hostaddr"] = nil
        return libpq(all)
    }

    private func libpq(_ values: [String: String]) -> String {
        var all = values
        if all["connect_timeout"] == nil { all["connect_timeout"] = "10" }
        return Self.order(all).map { "\($0.key)='\(Self.escape($0.value))'" }.joined(separator: " ")
    }

    /// A URL from form fields. Empty fields are left out.
    /// With `maskPassword`, the password shows as the mask, for the screen.
    public static func compose(scheme: String = "postgresql", host: String, port: String, database: String, user: String,
                               password: String, sslmode: String, maskPassword: Bool = false) -> String {
        var auth = encode(user)
        if !password.isEmpty { auth += ":" + (maskPassword ? mask : encode(password)) }
        if !auth.isEmpty { auth += "@" }
        let trimmedPort = port.trimmingCharacters(in: .whitespaces)
        let hostPart = host.contains(":") && !host.hasPrefix("[") ? "[\(host)]" : host
        var url = "\(scheme)://\(auth)\(hostPart)\(trimmedPort.isEmpty ? "" : ":" + trimmedPort)"
        if !database.isEmpty { url += "/" + encode(database) }
        if !sslmode.isEmpty { url += "?sslmode=" + sslmode }
        return url
    }

    // MARK: Parsing

    /// Strips what comes around a pasted URL: spaces, an .env name, quotes.
    static func clean(_ text: String) -> String {
        var value = text.trimmingCharacters(in: .whitespacesAndNewlines)
        if value.hasPrefix("export ") { value = String(value.dropFirst(7)) }
        // DATABASE_URL=postgres://… (only when what follows is a URL, so host=… stays)
        if let match = value.range(of: "^[A-Za-z_][A-Za-z0-9_]*\\s*=\\s*", options: .regularExpression) {
            let rest = value[match.upperBound...].trimmingCharacters(in: CharacterSet(charactersIn: "\"' "))
            if rest.range(of: "^(?i)(jdbc:)?[a-z][a-z0-9+.-]*:", options: .regularExpression) != nil {
                value = String(value[match.upperBound...])
            }
        }
        value = value.trimmingCharacters(in: .whitespaces)
        if value.count >= 2, let first = value.first, first == value.last, first == "\"" || first == "'" {
            value = String(value.dropFirst().dropLast())
        }
        return value.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    /// The part after the scheme, for postgres://, postgresql://, jdbc:… and postgresql+driver://.
    private static func urlForm(_ text: String) -> String? {
        guard let match = text.range(of: "^(?i)(jdbc:)?postgres(ql)?(\\+[a-z0-9_]+)?://", options: .regularExpression) else { return nil }
        return String(text[match.upperBound...])
    }

    static func parseURL(_ rest: String) -> [String: String] {
        var values: [String: String] = [:]
        var body = rest.split(separator: "#", maxSplits: 1, omittingEmptySubsequences: false).first.map(String.init) ?? ""
        var query = ""
        if let mark = body.firstIndex(of: "?") {
            query = String(body[body.index(after: mark)...])
            body = String(body[..<mark])
        }
        var authority = body
        if let slash = body.firstIndex(of: "/") {
            authority = String(body[..<slash])
            let path = String(body[body.index(after: slash)...])
            if !path.isEmpty { values["dbname"] = decode(path) }
        }
        if let at = authority.lastIndex(of: "@") {
            let userinfo = authority[..<at]
            authority = String(authority[authority.index(after: at)...])
            let parts = userinfo.split(separator: ":", maxSplits: 1, omittingEmptySubsequences: false)
            if let user = parts.first, !user.isEmpty { values["user"] = decode(String(user)) }
            if parts.count > 1 { values["password"] = decode(String(parts[1])) }
        }
        var hosts: [String] = []
        var ports: [String] = []
        for entry in authority.split(separator: ",", omittingEmptySubsequences: false).map(String.init) where !entry.isEmpty {
            if entry.hasPrefix("["), let close = entry.firstIndex(of: "]") {
                hosts.append(String(entry[entry.index(after: entry.startIndex)..<close]))
                let after = entry[entry.index(after: close)...]
                ports.append(after.hasPrefix(":") ? String(after.dropFirst()) : "")
            } else if let colon = entry.lastIndex(of: ":"), entry[entry.index(after: colon)...].allSatisfy(\.isNumber) {
                hosts.append(decode(String(entry[..<colon])))
                ports.append(String(entry[entry.index(after: colon)...]))
            } else {
                hosts.append(decode(entry))
                ports.append("")
            }
        }
        if !hosts.isEmpty, hosts.contains(where: { !$0.isEmpty }) { values["host"] = hosts.joined(separator: ",") }
        if ports.contains(where: { !$0.isEmpty }) { values["port"] = ports.joined(separator: ",") }
        for pair in query.split(separator: "&") {
            let kv = pair.split(separator: "=", maxSplits: 1, omittingEmptySubsequences: false)
            guard let key = kv.first, !key.isEmpty else { continue }
            values[decode(String(key))] = kv.count > 1 ? decode(String(kv[1])) : ""
        }
        return values
    }

    /// libpq's key=value form: host=x port=5432 password='it''s' (values may be quoted).
    private static func parseKeyValue(_ text: String) -> [String: String]? {
        var values: [String: String] = [:]
        var index = text.startIndex
        func skipSpaces() { while index < text.endIndex, text[index].isWhitespace { index = text.index(after: index) } }
        while true {
            skipSpaces()
            guard index < text.endIndex else { break }
            let keyStart = index
            while index < text.endIndex, text[index] != "=", !text[index].isWhitespace { index = text.index(after: index) }
            let key = String(text[keyStart..<index])
            skipSpaces()
            guard !key.isEmpty, index < text.endIndex, text[index] == "=" else { return nil }
            index = text.index(after: index)
            skipSpaces()
            var value = ""
            if index < text.endIndex, text[index] == "'" {
                index = text.index(after: index)
                var closed = false
                while index < text.endIndex {
                    let char = text[index]
                    if char == "\\", text.index(after: index) < text.endIndex {
                        index = text.index(after: index)
                        value.append(text[index])
                    } else if char == "'" {
                        closed = true
                        index = text.index(after: index)
                        break
                    } else {
                        value.append(char)
                    }
                    index = text.index(after: index)
                }
                guard closed else { return nil }
            } else {
                while index < text.endIndex, !text[index].isWhitespace {
                    value.append(text[index])
                    index = text.index(after: index)
                }
            }
            values[key] = value
        }
        return values.isEmpty ? nil : values
    }

    // MARK: Normalizing

    /// The settings libpq knows (PostgreSQL 16–18). Anything else is dropped.
    private static let libpqKeys: Set<String> = [
        "host", "hostaddr", "port", "dbname", "user", "password", "passfile", "channel_binding",
        "connect_timeout", "client_encoding", "options", "application_name", "fallback_application_name",
        "keepalives", "keepalives_idle", "keepalives_interval", "keepalives_count", "tcp_user_timeout",
        "replication", "gssencmode", "sslmode", "requiressl", "sslcompression", "sslcert", "sslkey",
        "sslpassword", "sslcertmode", "sslrootcert", "sslcrl", "sslcrldir", "sslsni", "requirepeer",
        "ssl_min_protocol_version", "ssl_max_protocol_version", "krbsrvname", "gsslib", "gssdelegation",
        "service", "target_session_attrs", "load_balance_hosts", "sslnegotiation", "require_auth",
    ]

    private static func normalize(_ input: [String: String]) -> [String: String] {
        var values: [String: String] = [:]
        var schema: String?
        for (rawKey, value) in input {
            let key = rawKey.lowercased()
            switch key {
            case "username": values["user"] = values["user"] ?? value
            case "database", "db": values["dbname"] = values["dbname"] ?? value
            case "applicationname": values["application_name"] = value
            case "schema", "currentschema", "search_path": schema = value
            case "ssl":
                if ["true", "1", "on", "require"].contains(value.lowercased()), input["sslmode"] == nil {
                    values["sslmode"] = "require"
                }
            case "sslmode":
                // node-postgres' no-verify: encrypted, certificate not checked = libpq's require.
                values["sslmode"] = value.lowercased() == "no-verify" ? "require" : value.lowercased()
            default:
                if libpqKeys.contains(key) { values[key] = value }
            }
        }
        if let schema, !schema.isEmpty {
            let option = "-c search_path=\(schema)"
            values["options"] = values["options"].map { "\($0) \(option)" } ?? option
        }
        return values
    }

    private static let preferred = ["host", "hostaddr", "port", "dbname", "user", "password", "sslmode"]

    private static func order(_ values: [String: String]) -> [(key: String, value: String)] {
        values.sorted { a, b in
            let ia = preferred.firstIndex(of: a.key) ?? preferred.count
            let ib = preferred.firstIndex(of: b.key) ?? preferred.count
            return ia == ib ? a.key < b.key : ia < ib
        }
    }

    private static func escape(_ value: String) -> String {
        value.replacingOccurrences(of: "\\", with: "\\\\").replacingOccurrences(of: "'", with: "\\'")
    }

    private static func decode(_ text: String) -> String { text.removingPercentEncoding ?? text }

    private static func encode(_ text: String) -> String {
        text.addingPercentEncoding(withAllowedCharacters: CharacterSet(charactersIn:
            "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~")) ?? text
    }
}

/// Quotes a name for SQL: my"table -> "my""table"
public func quoteIdent(_ name: String) -> String {
    "\"" + name.replacingOccurrences(of: "\"", with: "\"\"") + "\""
}

/// The parts of any scheme://user:password@host:port/path?query URL, decoded.
/// Postgres has its own parser (ConnectionURL); the other engines start here.
public struct URLParts: Sendable, Hashable {
    /// Lowercased, without "jdbc:" or a "+driver" suffix: "mysql", "redis", "mongodb+srv".
    public let scheme: String
    public let user: String?
    public let password: String?
    /// Every host:port pair; port is nil when not given.
    public let hosts: [Host]
    /// The path without its leading "/": the database name, a Redis index, a file.
    public let path: String
    /// Query parameters, keys as written.
    public let query: [String: String]

    public struct Host: Sendable, Hashable {
        public let name: String
        public let port: Int?
    }

    /// nil when the text is not scheme://… (after cleaning an .env line or quotes).
    public init?(_ text: String) {
        let cleaned = ConnectionURL.clean(text)
        guard let match = cleaned.range(of: "^(?i)(jdbc:)?([a-z][a-z0-9.-]*)(\\+[a-z0-9_]+)?://", options: .regularExpression)
        else { return nil }
        let head = cleaned[match].lowercased()
        var scheme = head.replacingOccurrences(of: "jdbc:", with: "").replacingOccurrences(of: "://", with: "")
        if let plus = scheme.firstIndex(of: "+"), !scheme.hasPrefix("mongodb+srv") { scheme = String(scheme[..<plus]) }
        let rest = String(cleaned[match.upperBound...])
        // Reuse the Postgres parser for the authority and query; it keeps hosts and ports aligned.
        let values = ConnectionURL.parseURL(rest)
        self.scheme = scheme
        user = values["user"]
        password = values["password"]
        let names = (values["host"] ?? "").split(separator: ",", omittingEmptySubsequences: false).map(String.init)
        let ports = (values["port"] ?? "").split(separator: ",", omittingEmptySubsequences: false).map(String.init)
        hosts = names.enumerated().compactMap { index, name in
            name.isEmpty && names.count == 1 ? nil : Host(name: name, port: index < ports.count ? Int(ports[index]) : nil)
        }
        path = values["dbname"] ?? ""
        var query = values
        for key in ["user", "password", "host", "port", "dbname"] { query[key] = nil }
        self.query = query
    }
}

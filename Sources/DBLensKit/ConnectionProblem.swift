import Foundation

/// Why a connection failed, in one clear sentence, with the server's own
/// message under it. Drivers word the same failure differently (and libpq
/// repeats it for every address it tried); this names the cause the same
/// way for every database: refused, unknown host, timeout, credentials,
/// missing database, TLS.
public enum ConnectionProblem {
    public static func explain(_ error: Error, url text: String, viaSSH: Bool) -> DatabaseError {
        let raw = (error as? DatabaseError)?.message ?? error.localizedDescription
        let detail = clean(raw)
        guard let headline = headline(for: raw, summary: Connector.summary(text), url: text, viaSSH: viaSSH) else {
            return DatabaseError(message: detail)
        }
        return DatabaseError(message: headline + "\n\n" + detail)
    }

    static func headline(for raw: String, summary: Connector.Summary?, url text: String, viaSSH: Bool) -> String? {
        let message = raw.lowercased()
        func has(_ needles: String...) -> Bool { needles.contains { message.contains($0.lowercased()) } }
        // Mongo's host may carry its port ("db:27017"); one host is enough for the sentence.
        var host = summary?.host ?? "localhost"
        if summary?.port == nil, let colon = host.lastIndex(of: ":"), !host.contains("]") {
            host = String(host[..<colon])
        }
        let place = summary?.port.map { "\(host):\($0)" } ?? host
        let user = summary?.user ?? ""
        let database = (summary?.database ?? "").replacingOccurrences(of: "db ", with: "")
        let tunnel = viaSSH ? String(localized: " (through the SSH tunnel)") : ""
        let tlsURL = text.lowercased().hasPrefix("rediss://") || has("sslmode=require", "tls=true", "ssl=true")

        if has("ssl", "tls"), has("not support", "não oferece suporte", "does not support", "wrong version", "handshake", "requires") {
            return String(localized: "TLS didn't work with \(place): the server and the URL disagree on encryption. Check the URL's TLS/SSL setting.")
        }
        let local = ["localhost", "127.0.0.1", "::1"].contains(host)
        let defaultPort = summary.map { Self.defaultPort($0.engine) } ?? 0
        let docker = String(localized: "If the database runs in Docker, publish its port on the server (ports: in Docker Compose, or the External Port field in panels like Dokploy) and put that port in the URL.")
        let sshHost = String(localized: "Through SSH, the URL's host and port are seen from the SSH server: use the address the database has there (127.0.0.1 and a published port, or the container's IP), not this Mac's.")
        if has("refused", "(61)", "(36)") {
            var hints: [String] = []
            if local, !viaSSH {
                hints.append(String(localized: "Is the database running on this Mac?"))
            } else if !viaSSH {
                hints.append(docker)
            }
            if viaSSH { hints.append(sshHost) }
            if let port = summary?.port, defaultPort > 0, port != defaultPort {
                hints.append(String(localized: "The URL uses port \(String(port)); this database's usual port is \(String(defaultPort))."))
            }
            return Self.compose(String(localized: "Nothing answered at \(place)\(tunnel): the connection was refused."), hints)
        }
        if has("invalid srv service name") {
            return String(localized: "\"\(host)\" isn't a valid cluster name for mongodb+srv://. Check the URL's host.")
        }
        if has("could not translate host", "traduzir o nome", "unknown server host", "nosuchrecord", "failed to resolve",
               "nodename nor servname", "failed to look up srv", "host not found", "name or service not known") {
            // A name without a dot (a Docker service, a LAN machine) only resolves inside its own network.
            if !host.contains("."), !host.contains(":"), host != "localhost" {
                let hint = viaSSH
                    ? String(localized: "Through SSH the name must resolve on the SSH server, and container names usually don't resolve on the server itself: use the container's IP, or 127.0.0.1 and a published port.")
                    : String(localized: "From this Mac, use the server's IP or name instead, with the database's published port (ports: in Docker Compose, or the External Port field in panels like Dokploy), or connect through SSH to that server.")
                return Self.compose(String(localized: "The host \"\(host)\" wasn't found. It looks like a container or service name, which only resolves inside its own Docker network."), [hint])
            }
            return Self.compose(String(localized: "The host \"\(host)\" wasn't found."),
                                [String(localized: "Check the name, and whether this Mac can reach that network (some need a VPN).")])
        }
        if has("timeout", "tempo limite", "timed out", "(60)", "within") {
            if tlsURL, text.lowercased().hasPrefix("rediss://") {
                return String(localized: "\(place) didn't answer TLS. If the server doesn't use TLS, use redis:// instead of rediss://.")
            }
            var hints = [String(localized: "A firewall may be blocking port \(String(summary?.port ?? defaultPort)). If you open it, allow only your private network or VPN, not the whole internet.")]
            if !viaSSH { hints.append(String(localized: "Or connect through SSH (Edit Connection > SSH) via a machine that reaches the database.")) }
            if viaSSH { hints.append(sshHost) }
            return Self.compose(String(localized: "\(place)\(tunnel) didn't answer in time."), hints)
        }
        if has("password authentication failed", "autenticação do tipo senha", "access denied for user", "wrongpass",
               "authentication failed", "invalid username-password", "invalid password") {
            return user.isEmpty
                ? String(localized: "The server refused the login: wrong password, or a user that can't log in.")
                : String(localized: "The server refused the login for \"\(user)\": wrong password, or a user that can't log in.")
        }
        if has("noauth") {
            return String(localized: "The server asks for a password, and the URL has none.")
        }
        if has("no pg_hba.conf entry", "nenhuma entrada no pg_hba.conf") {
            return String(localized: "The server doesn't accept connections from this machine for this user and database (pg_hba.conf).")
        }
        if has("role \"", "função \"", "papel \""), has("does not exist", "não existe") {
            return String(localized: "The user \"\(user)\" doesn't exist on this server.")
        }
        if has("database \"", "banco de dados \"", "unknown database"), has("does not exist", "não existe", "unknown database") {
            return String(localized: "The database \"\(database)\" doesn't exist on this server.")
        }
        if has("db index is out of range") {
            return String(localized: "This Redis server has no database \(database); it has fewer numbered databases.")
        }
        if has("file is not a database") {
            return String(localized: "This file isn't a SQLite database.")
        }
        return nil
    }

    static func defaultPort(_ engine: Engine) -> Int {
        switch engine {
        case .postgres: 5432
        case .mysql: 3306
        case .redis: 6379
        case .mongodb: 27017
        case .sqlite: 0
        }
    }

    /// The cause, then what to try, one per line.
    private static func compose(_ headline: String, _ hints: [String]) -> String {
        ([headline] + hints.map { "• " + $0 }).joined(separator: "\n")
    }

    /// The server's message on one or two lines: libpq's repeats for each
    /// address it tried (::1, then 127.0.0.1) and its tab-indented hints go.
    static func clean(_ raw: String) -> String {
        var lines: [String] = []
        for line in raw.split(separator: "\n").map({ $0.trimmingCharacters(in: .whitespaces) }) where !line.isEmpty {
            let core = line.replacingOccurrences(of: #"\([^)]*\)"#, with: "", options: .regularExpression)
            if lines.contains(where: { $0.replacingOccurrences(of: #"\([^)]*\)"#, with: "", options: .regularExpression) == core }) { continue }
            lines.append(line)
        }
        return lines.prefix(3).joined(separator: "\n")
    }
}

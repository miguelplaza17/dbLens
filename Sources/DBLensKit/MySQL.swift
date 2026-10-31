import CMariaDB
import Foundation
import Synchronization

/// A MySQL or MariaDB connection, as pasted: any of the usual ways people write it.
///   mysql://user:pass@host:3306/shop?ssl-mode=REQUIRED   (also mariadb://, mysqlx://)
///   jdbc:mysql://host/shop?user=ana&password=…&useSSL=true
///   jdbc:mariadb://host/shop?sslMode=verify-full
///   mysql+pymysql://…  mysql+mysqldb://…                   (SQLAlchemy)
///   DATABASE_URL="mysql://…"                                (an .env line)
///   mysql://ana@localhost/shop?socket=/tmp/mysql.sock      (local socket)
/// Unknown parameters are ignored.
public struct MySQLURL: Sendable, Hashable {
    /// What was pasted, without an .env name or quotes around it.
    public let raw: String
    /// "localhost" when the URL names none.
    public let host: String
    /// nil means the default, 3306 (also for mysqlx://, whose own port is for the X protocol).
    public let port: Int?
    public let user: String?
    public let password: String?
    /// nil when the URL names none.
    public let database: String?
    /// A Unix socket path, for local servers.
    public let socket: String?
    /// DISABLED, PREFERRED, REQUIRED, VERIFY_CA or VERIFY_IDENTITY; nil means PREFERRED.
    public let sslMode: String?
    /// The charset asked for. The connection always uses utf8mb4, so text reads as UTF-8.
    public let charset: String?

    /// "shop @ db.example.com" for the sidebar.
    public var suggestedName: String { "\(database ?? "mysql") @ \(host)" }

    private static let schemes: Set<String> = ["mysql", "mariadb", "mysqlx"]

    public init(_ text: String) throws {
        let cleaned = ConnectionURL.clean(text)
        guard !cleaned.isEmpty else { throw DatabaseError(message: String(localized: "Paste the database URL.")) }
        guard let parts = URLParts(cleaned), Self.schemes.contains(parts.scheme) else {
            throw DatabaseError(message: String(localized: "Unrecognized format. Use a mysql://… URL (or mariadb://, jdbc:mysql://)"))
        }
        let query = Dictionary(parts.query.map { ($0.key.lowercased(), $0.value) }, uniquingKeysWith: { a, _ in a })
        func nonEmpty(_ value: String?) -> String? { value.flatMap { $0.isEmpty ? nil : $0 } }

        raw = cleaned
        let first = parts.hosts.first
        var socket = nonEmpty(query["socket"] ?? query["unix_socket"])
        if let name = first?.name, name.hasPrefix("/") {
            // mysql://ana@%2Ftmp%2Fmysql.sock/shop
            socket = socket ?? name
            host = "localhost"
        } else {
            host = nonEmpty(first?.name) ?? "localhost"
        }
        self.socket = socket
        port = first?.port
        user = nonEmpty(parts.user ?? query["user"] ?? query["username"])
        password = nonEmpty(parts.password ?? query["password"])
        database = nonEmpty(parts.path) ?? nonEmpty(query["database"] ?? query["dbname"])
        sslMode = Self.sslMode(query)
        charset = nonEmpty(query["charset"] ?? query["characterencoding"])
    }

    /// ssl-mode, sslmode (also JDBC's sslMode), ssl=true and useSSL=true, in MySQL's own words.
    private static func sslMode(_ query: [String: String]) -> String? {
        if let mode = query["ssl-mode"] ?? query["sslmode"] ?? query["ssl_mode"] {
            switch mode.lowercased().replacingOccurrences(of: "-", with: "_") {
            case "disabled", "disable", "false", "0", "off": return "DISABLED"
            case "preferred", "prefer", "allow": return "PREFERRED"
            case "required", "require", "true", "1", "on", "trust", "no_verify": return "REQUIRED"
            case "verify_ca": return "VERIFY_CA"
            case "verify_identity", "verify_full": return "VERIFY_IDENTITY"
            default: return nil
            }
        }
        guard let flag = (query["ssl"] ?? query["usessl"] ?? query["requiressl"])?.lowercased() else { return nil }
        if ["false", "0", "off", "no"].contains(flag) { return "DISABLED" }
        guard ["true", "1", "on", "yes", "require", "required"].contains(flag) else { return nil }
        return query["verifyservercertificate"]?.lowercased() == "true" ? "VERIFY_CA" : "REQUIRED"
    }
}

/// Where and how to connect: what both the session and its cancel connection use.
struct MySQLTarget: Sendable {
    var host: String
    var port: Int
    var user: String?
    var password: String?
    var database: String?
    var socket: String?
    var sslMode: String?
    var forceTCP: Bool

    init(_ url: MySQLURL, localPort: Int?) {
        if let localPort {
            host = "127.0.0.1"
            port = localPort
            socket = nil
            forceTCP = true
        } else {
            host = url.host
            port = url.port ?? 3306
            socket = url.socket
            // "localhost" means the socket to the client library; a given port means TCP.
            forceTCP = url.host == "localhost" && url.socket == nil
        }
        user = url.user
        password = url.password
        database = url.database
        sslMode = url.sslMode
    }

    /// Once per process, before any connection.
    private static let libraryReady: Void = { _ = mysql_server_init(0, nil, nil) }()

    /// Connector/C's code for a failed TLS handshake or a server without TLS.
    private static let tlsFailed: UInt32 = 2026

    /// Opens a connection, or throws the server's own message. PREFERRED (the
    /// default) tries TLS first and falls back to plain TCP when TLS fails.
    func open() throws -> UnsafeMutablePointer<MYSQL> {
        _ = Self.libraryReady
        do {
            switch sslMode {
            case "DISABLED": return try connect(tls: 0, verify: 0)
            case "REQUIRED": return try connect(tls: 1, verify: 0)
            case "VERIFY_CA", "VERIFY_IDENTITY": return try connect(tls: 1, verify: 1)
            default:
                do {
                    return try connect(tls: 1, verify: 0)
                } catch let failure as Failure where failure.code == Self.tlsFailed {
                    return try connect(tls: 0, verify: 0)
                }
            }
        } catch let failure as Failure {
            throw failure.error
        }
    }

    /// A failed connection, with Connector/C's error code.
    private struct Failure: Error {
        let code: UInt32
        let error: DatabaseError
    }

    /// The app's copy of the client plugins, or nil outside the app (tests use Homebrew's).
    private static let bundledPlugins: String? = {
        guard let dir = Bundle.main.privateFrameworksURL?.appending(path: "mariadb-plugins"),
              FileManager.default.fileExists(atPath: dir.path) else { return nil }
        return dir.path
    }()

    private func connect(tls: my_bool, verify: my_bool) throws -> UnsafeMutablePointer<MYSQL> {
        guard let conn = mysql_init(nil) else {
            throw Failure(code: 0, error: DatabaseError(message: String(localized: "Out of memory while connecting.")))
        }
        var timeout: UInt32 = 10
        mysql_options(conn, MYSQL_OPT_CONNECT_TIMEOUT, &timeout)
        // Login methods like MySQL 8's caching_sha2_password are plugins: inside the
        // app they sit in Frameworks/mariadb-plugins (tools/bundle_libs.sh puts them there).
        if let plugins = Self.bundledPlugins {
            _ = plugins.withCString { mysql_options(conn, MYSQL_PLUGIN_DIR, $0) }
        }
        _ = "utf8mb4".withCString { mysql_options(conn, MYSQL_SET_CHARSET_NAME, $0) }
        if forceTCP {
            var tcp = MYSQL_PROTOCOL_TCP.rawValue
            mysql_options(conn, MYSQL_OPT_PROTOCOL, &tcp)
        }
        var enforce = tls
        var verify = verify
        mysql_options(conn, MYSQL_OPT_SSL_ENFORCE, &enforce)
        mysql_options(conn, MYSQL_OPT_SSL_VERIFY_SERVER_CERT, &verify)
        let flags = UInt(CLIENT_MULTI_STATEMENTS) | UInt(CLIENT_FOUND_ROWS)
        guard mysql_real_connect(conn, host, user, password, database, UInt32(port), socket, flags) != nil else {
            let message = String(cString: mysql_error(conn)).trimmingCharacters(in: .whitespacesAndNewlines)
            let code = mysql_errno(conn)
            mysql_close(conn)
            throw Failure(code: code, error: DatabaseError(message: message.isEmpty ? String(localized: "Couldn't connect.") : message))
        }
        return conn
    }
}

/// One open MySQL or MariaDB connection. Connector/C blocks, so every call runs
/// on the session's own serial queue, never on the main thread or the shared pool.
public actor MySQLSession: DatabaseSession {
    public nonisolated var engine: Engine { .mysql }
    /// `name` with backticks; a backtick inside is doubled.
    public nonisolated func quote(_ identifier: String) -> String {
        "`" + identifier.replacingOccurrences(of: "`", with: "``") + "`"
    }

    private let queue = DispatchSerialQueue(label: "dbLens.mariadb")
    public nonisolated var unownedExecutor: UnownedSerialExecutor { queue.asUnownedSerialExecutor() }

    /// Only touched on `queue`, and by deinit once nothing else can.
    nonisolated(unsafe) private let conn: UnsafeMutablePointer<MYSQL>
    private let canceller: MySQLCanceller
    public nonisolated let url: MySQLURL
    public nonisolated let serverVersion: String
    /// The SSH tunnel this connection goes through, if any. It closes with the session.
    public nonisolated let tunnel: SSHTunnel?

    /// Connects, through an SSH tunnel to the database's host and port (as the SSH
    /// server sees them) when `ssh` is given, or throws the server's own message.
    public static func connect(_ url: MySQLURL, ssh: SSHTunnel.Config?) async throws -> MySQLSession {
        guard let ssh else { return try await Task.detached { try MySQLSession(url: url, tunnel: nil) }.value }
        let tunnel = try await SSHTunnel.open(ssh, to: url.host, port: url.port ?? 3306)
        do {
            return try await Task.detached { try MySQLSession(url: url, tunnel: tunnel) }.value
        } catch {
            tunnel.close()
            throw error
        }
    }

    private init(url: MySQLURL, tunnel: SSHTunnel?) throws {
        let target = MySQLTarget(url, localPort: tunnel?.localPort)
        let conn = try target.open()
        self.conn = conn
        self.url = url
        self.tunnel = tunnel
        canceller = MySQLCanceller(target: target, threadID: mysql_thread_id(conn))
        serverVersion = Self.version(String(cString: mysql_get_server_info(conn)))
    }

    deinit {
        mysql_close(conn)
        tunnel?.close()
    }

    /// "8.4.3", or "11.4.2-MariaDB" without the distribution suffix.
    static func version(_ info: String) -> String {
        guard let range = info.range(of: "-MariaDB") else { return info }
        return String(info[..<range.upperBound])
    }

    // MARK: Running SQL

    /// Runs one or more statements, the way the mysql client does.
    public func execute(_ sql: String) throws -> Execution {
        let clock = ContinuousClock()
        let start = clock.now
        let results = try runAll(sql)
        return Execution(results: results, duration: clock.now - start)
    }

    /// Stops the running query: a second, short connection sends KILL QUERY.
    /// Safe to call from any thread; it never waits for the server.
    public nonisolated func cancel() { canceller.cancel() }

    private func runAll(_ sql: String) throws -> [QueryResult] {
        canceller.begin()
        var results: [QueryResult] = []
        var failure: DatabaseError?
        let verbs = Self.statementVerbs(sql)
        if mysql_real_query(conn, sql, UInt(sql.utf8.count)) != 0 {
            failure = serverError()
        } else {
            var index = 0
            while true {
                let verb = index < verbs.count ? verbs[index] : (verbs.last ?? "")
                if let res = mysql_store_result(conn) {
                    results.append(Self.read(res))
                    mysql_free_result(res)
                    // A procedure's result sets come before its own status.
                    if verb != "CALL" { index += 1 }
                } else if mysql_field_count(conn) == 0 {
                    results.append(Self.status(verb: verb, affected: mysql_affected_rows(conn)))
                    index += 1
                } else {
                    failure = serverError()
                    break
                }
                let next = mysql_next_result(conn)
                if next > 0 { failure = serverError() }
                if next != 0 { break }
            }
        }
        let canceled = canceller.end()
        if let failure { throw failure }
        // SLEEP() and some statements end quietly when killed; the run was still stopped.
        if canceled { throw DatabaseError(message: String(localized: "The query was canceled.")) }
        return results
    }

    private func serverError() -> DatabaseError {
        let message = String(cString: mysql_error(conn)).trimmingCharacters(in: .whitespacesAndNewlines)
        return DatabaseError(message: message.isEmpty ? String(localized: "The server closed the connection.") : message)
    }

    private func single(_ sql: String) throws -> QueryResult {
        let results = try runAll(sql)
        guard let main = results.last(where: \.hasRows) ?? results.last else { throw DatabaseError(message: String(localized: "No result.")) }
        return main
    }

    /// Statements whose tag carries the affected rows.
    private static let countedVerbs: Set<String> = ["INSERT", "UPDATE", "DELETE", "REPLACE"]

    private static func status(verb: String, affected: UInt64) -> QueryResult {
        let count = affected == UInt64.max ? 0 : Int(affected)
        let command = countedVerbs.contains(verb) ? "\(verb) \(count)" : (verb.isEmpty ? "OK" : verb)
        return QueryResult(columns: [], rows: [], command: command, totalRows: count)
    }

    private static func read(_ res: UnsafeMutablePointer<MYSQL_RES>) -> QueryResult {
        let count = Int(mysql_num_fields(res))
        let fields = mysql_fetch_fields(res).map { pointer in (0..<count).map { pointer[$0] } } ?? []
        let columns = fields.map { Column(name: String(cString: $0.name), type: typeName($0)) }
        let binary = fields.map(isBinary)
        let total = Int(mysql_num_rows(res))
        let kept = min(total, QueryResult.rowLimit)
        var rows: [[String?]] = []
        rows.reserveCapacity(kept)
        while rows.count < kept, let row = mysql_fetch_row(res), let lengths = mysql_fetch_lengths(res) {
            rows.append((0..<count).map { column in
                guard let value = row[column] else { return nil }
                let bytes = UnsafeRawBufferPointer(start: value, count: Int(lengths[column]))
                return binary[column] ? hex(bytes) : String(decoding: bytes, as: UTF8.self)
            })
        }
        return QueryResult(columns: columns, rows: rows, command: "SELECT \(total)", truncated: total > kept, totalRows: total)
    }

    /// Bytes, not text: BLOB, BINARY, VARBINARY, BIT and geometry in the binary charset.
    private static func isBinary(_ field: MYSQL_FIELD) -> Bool {
        guard field.charsetnr == 63 else { return false }
        switch field.type {
        case MYSQL_TYPE_TINY_BLOB, MYSQL_TYPE_MEDIUM_BLOB, MYSQL_TYPE_LONG_BLOB, MYSQL_TYPE_BLOB,
             MYSQL_TYPE_STRING, MYSQL_TYPE_VAR_STRING, MYSQL_TYPE_VARCHAR, MYSQL_TYPE_BIT, MYSQL_TYPE_GEOMETRY:
            return true
        default:
            return false
        }
    }

    /// Bytes shown as 0x…, cut after this many, so the grid stays fast.
    static let hexLimit = 64

    static func hex(_ bytes: UnsafeRawBufferPointer) -> String {
        let digits = Array("0123456789ABCDEF".utf8)
        var text: [UInt8] = Array("0x".utf8)
        text.reserveCapacity(2 + min(bytes.count, hexLimit) * 2)
        for byte in bytes.prefix(hexLimit) {
            text.append(digits[Int(byte >> 4)])
            text.append(digits[Int(byte & 0xF)])
        }
        return String(decoding: text, as: UTF8.self) + (bytes.count > hexLimit ? "…" : "")
    }

    /// The type of a result column: "int", "varchar", "datetime", "json"…
    private static func typeName(_ field: MYSQL_FIELD) -> String {
        if field.flags & UInt32(ENUM_FLAG) != 0 { return "enum" }
        if field.flags & UInt32(SET_FLAG) != 0 { return "set" }
        let binary = field.charsetnr == 63
        switch field.type {
        case MYSQL_TYPE_TINY: return "tinyint"
        case MYSQL_TYPE_SHORT: return "smallint"
        case MYSQL_TYPE_INT24: return "mediumint"
        case MYSQL_TYPE_LONG: return "int"
        case MYSQL_TYPE_LONGLONG: return "bigint"
        case MYSQL_TYPE_FLOAT: return "float"
        case MYSQL_TYPE_DOUBLE: return "double"
        case MYSQL_TYPE_DECIMAL, MYSQL_TYPE_NEWDECIMAL: return "decimal"
        case MYSQL_TYPE_NULL: return "null"
        case MYSQL_TYPE_TIMESTAMP, MYSQL_TYPE_TIMESTAMP2: return "timestamp"
        case MYSQL_TYPE_DATE, MYSQL_TYPE_NEWDATE: return "date"
        case MYSQL_TYPE_TIME, MYSQL_TYPE_TIME2: return "time"
        case MYSQL_TYPE_DATETIME, MYSQL_TYPE_DATETIME2: return "datetime"
        case MYSQL_TYPE_YEAR: return "year"
        case MYSQL_TYPE_VARCHAR, MYSQL_TYPE_VAR_STRING: return binary ? "varbinary" : "varchar"
        case MYSQL_TYPE_STRING: return binary ? "binary" : "char"
        case MYSQL_TYPE_BIT: return "bit"
        case MYSQL_TYPE_JSON: return "json"
        case MYSQL_TYPE_ENUM: return "enum"
        case MYSQL_TYPE_SET: return "set"
        case MYSQL_TYPE_TINY_BLOB, MYSQL_TYPE_MEDIUM_BLOB, MYSQL_TYPE_LONG_BLOB, MYSQL_TYPE_BLOB: return binary ? "blob" : "text"
        case MYSQL_TYPE_GEOMETRY: return "geometry"
        case MYSQL_TYPE_VECTOR: return "vector"
        default: return ""
        }
    }

    /// The first word of each statement, uppercased, skipping comments and
    /// quoted text: "insert …; select …" -> ["INSERT", "SELECT"].
    static func statementVerbs(_ sql: String) -> [String] {
        let bytes = Array(sql.utf8)
        var verbs: [String] = []
        var verb: [UInt8] = []
        var started = false
        var collecting = false
        var index = 0
        func skip(to terminator: [UInt8]) {
            while index < bytes.count, !bytes[index...].starts(with: terminator) { index += 1 }
            index = min(bytes.count, index + terminator.count)
        }
        while index < bytes.count {
            let byte = bytes[index]
            let next = index + 1 < bytes.count ? bytes[index + 1] : 0
            switch byte {
            case UInt8(ascii: "-") where next == UInt8(ascii: "-")
                && (index + 2 >= bytes.count || [9, 10, 13, 32].contains(bytes[index + 2])):
                skip(to: [10])
                collecting = false
            case UInt8(ascii: "#"):
                skip(to: [10])
                collecting = false
            case UInt8(ascii: "/") where next == UInt8(ascii: "*"):
                index += 2
                skip(to: Array("*/".utf8))
                collecting = false
            case UInt8(ascii: "'"), UInt8(ascii: "\""), UInt8(ascii: "`"):
                started = true
                collecting = false
                index += 1
                while index < bytes.count {
                    if bytes[index] == UInt8(ascii: "\\"), byte != UInt8(ascii: "`") { index += 2; continue }
                    if bytes[index] == byte {
                        if index + 1 < bytes.count, bytes[index + 1] == byte { index += 2; continue }
                        break
                    }
                    index += 1
                }
                index += 1
            case UInt8(ascii: ";"):
                if started { verbs.append(String(decoding: verb, as: UTF8.self).uppercased()) }
                started = false
                collecting = false
                verb = []
                index += 1
            case 9, 10, 13, 32:
                collecting = false
                index += 1
            default:
                if !started {
                    started = true
                    collecting = true
                }
                let isLetter = (byte >= 65 && byte <= 90) || (byte >= 97 && byte <= 122)
                if collecting, isLetter { verb.append(byte) } else { collecting = false }
                index += 1
            }
        }
        if started { verbs.append(String(decoding: verb, as: UTF8.self).uppercased()) }
        return verbs
    }

    // MARK: Browsing

    /// Tables and views in every database the user can see, the URL's database first.
    public func relations() throws -> [Relation] {
        let sql = """
            select TABLE_SCHEMA, TABLE_NAME, TABLE_TYPE, TABLE_ROWS
            from information_schema.TABLES
            where TABLE_SCHEMA not in ('mysql', 'information_schema', 'performance_schema', 'sys')
            """
        let relations = try single(sql).rows.map { row in
            let kind: Relation.Kind = (row[2] ?? "").hasSuffix("VIEW") ? .view : .table
            return Relation(schema: row[0] ?? "", name: row[1] ?? "", kind: kind, estimate: Double(row[3] ?? "") ?? -1)
        }
        let first = url.database
        return relations.sorted { a, b in
            if (a.schema == first) != (b.schema == first) { return a.schema == first }
            return a.schema == b.schema ? a.name < b.name : a.schema < b.schema
        }
    }

    /// One page of a table's rows, with the column types and, for tables with a
    /// primary key, the row keys. Sorted by the primary key, so pages and edited
    /// rows stay in place. Tables without one are read-only.
    public func page(of relation: Relation, limit: Int, offset: Int, search: String = "", sort: ColumnSort? = nil) throws -> QueryResult {
        let primaryKey = try primaryKey(of: relation)
        // MySQL sorts NULL first going up; "x is null" puts it last either way.
        let terms = (sort.map { ["\(quote($0.column)) is null", "\(quote($0.column)) \($0.direction)"] } ?? []) + primaryKey.map(quote)
        let order = terms.isEmpty ? "" : " order by " + terms.joined(separator: ", ")
        let types = try columnTypes(of: relation)
        var result = try single("select * from \(qualified(relation))\(filter(search, columns: types.map { $0[0] ?? "" }))\(order) limit \(max(limit, 0)) offset \(max(offset, 0))")
        let indexes = primaryKey.compactMap { name in result.columns.firstIndex { $0.name == name } }
        if !primaryKey.isEmpty, indexes.count == primaryKey.count {
            result.keys = RowKeys(columns: primaryKey, kind: .primaryKey,
                                  values: result.rows.map { row in indexes.map { row[$0] } })
        }
        let byName = Dictionary(types.map { ($0[0] ?? "", $0[1] ?? "") }, uniquingKeysWith: { a, _ in a })
        result.columns = result.columns.map { Column(name: $0.name, type: byName[$0.name] ?? $0.type) }
        return result
    }

    /// The primary-key columns of a table, in key order. Empty for views and keyless tables.
    public func primaryKey(of relation: Relation) throws -> [String] {
        guard relation.kind == .table else { return [] }
        return try single("""
            select COLUMN_NAME from information_schema.KEY_COLUMN_USAGE
            where TABLE_SCHEMA = \(literal(relation.schema)) and TABLE_NAME = \(literal(relation.name))
              and CONSTRAINT_NAME = 'PRIMARY'
            order by ORDINAL_POSITION
            """).rows.compactMap { $0.first ?? nil }
    }

    /// Exact row count, or nil when the table is big enough that counting is slow.
    public func count(of relation: Relation, search: String = "") throws -> Int? {
        if relation.estimate > 500_000 { return nil }
        let where_ = search.isEmpty ? "" : filter(search, columns: try columnTypes(of: relation).map { $0[0] ?? "" })
        return Int(try single("select count(*) from \(qualified(relation))\(where_)").rows.first?.first.flatMap { $0 } ?? "")
    }

    /// Column name and type, in table order.
    private func columnTypes(of relation: Relation) throws -> [[String?]] {
        try single("""
            select COLUMN_NAME, COLUMN_TYPE from information_schema.COLUMNS
            where TABLE_SCHEMA = \(literal(relation.schema)) and TABLE_NAME = \(literal(relation.name))
            order by ORDINAL_POSITION
            """).rows
    }

    /// Every column joined into one text (NULLs skipped) and matched with LIKE.
    private func filter(_ search: String, columns: [String]) -> String {
        guard !search.isEmpty, !columns.isEmpty else { return "" }
        return " where concat_ws(' ', \(columns.map(quote).joined(separator: ", "))) like \(literal(likePattern(search)))"
    }

    // MARK: Editing

    /// Changes one row. `changes` maps column names to new text (nil is NULL);
    /// the server converts the text to each column's type.
    public func update(_ relation: Relation, keys: RowKeys, row: Int, changes: [(String, String?)]) throws -> EditOutcome {
        guard !changes.isEmpty else { return EditOutcome(sql: "", command: "UPDATE 0", affected: 0) }
        let set = assignments(changes, separator: ", ")
        let filter = whereClause(keys, row: row)
        let target = qualified(relation)
        return try edit(SQL(sent: "update \(target) set \(set.sent) where \(filter.sent)",
                            logged: "update \(target) set \(set.logged) where \(filter.logged)"), expectOne: true)
    }

    /// Adds a row. Columns left out get their default.
    public func insert(_ relation: Relation, values: [(String, String?)]) throws -> EditOutcome {
        let names = values.map { quote($0.0) }.joined(separator: ", ")
        let sent = values.map { literal($0.1) }.joined(separator: ", ")
        let logged = Array(repeating: "?", count: values.count).joined(separator: ", ")
        let head = "insert into \(qualified(relation)) (\(names)) values "
        return try edit(SQL(sent: head + "(\(sent))", logged: head + "(\(logged))"), expectOne: false)
    }

    public func delete(_ relation: Relation, keys: RowKeys, row: Int) throws -> EditOutcome {
        let filter = whereClause(keys, row: row)
        let head = "delete from \(qualified(relation)) where "
        return try edit(SQL(sent: head + filter.sent, logged: head + filter.logged), expectOne: true)
    }

    /// The statement sent, with values, and the one logged, with ? in their place.
    private struct SQL {
        let sent: String
        let logged: String
    }

    private func assignments(_ pairs: [(String, String?)], separator: String) -> SQL {
        SQL(sent: pairs.map { "\(quote($0.0)) = \(literal($0.1))" }.joined(separator: separator),
            logged: pairs.map { "\(quote($0.0)) = ?" }.joined(separator: separator))
    }

    private func whereClause(_ keys: RowKeys, row: Int) -> SQL {
        let values = keys.values[row]
        let pairs = keys.columns.enumerated().map { ($0.element, $0.offset < values.count ? values[$0.offset] : nil) }
        let nulls = pairs.filter { $0.1 == nil }.map { "\(quote($0.0)) is null" }
        let set = assignments(pairs.filter { $0.1 != nil }, separator: " and ")
        let join = { (part: String) in ([part].filter { !$0.isEmpty } + nulls).joined(separator: " and ") }
        return SQL(sent: join(set.sent), logged: join(set.logged))
    }

    /// Runs one statement. An edit meant for exactly one row is rolled back if
    /// it would touch more, and refused if it touches none.
    private func edit(_ sql: SQL, expectOne: Bool) throws -> EditOutcome {
        if expectOne { _ = try runAll("start transaction") }
        do {
            let result = try single(sql.sent)
            let affected = Int(result.command.split(separator: " ").last ?? "") ?? 0
            if expectOne {
                guard affected <= 1 else {
                    _ = try? runAll("rollback")
                    throw DatabaseError(message: String(localized: "The change would have affected \(affected) rows and was rolled back."))
                }
                guard affected == 1 else {
                    _ = try? runAll("rollback")
                    throw DatabaseError(message: String(localized: "The row changed or was deleted since it was loaded. Reload the table and try again."))
                }
                _ = try runAll("commit")
            }
            return EditOutcome(sql: sql.logged, command: result.command, affected: affected)
        } catch {
            if expectOne { _ = try? runAll("rollback") }
            throw error
        }
    }

    /// A quoted string literal, escaped by Connector/C for this connection's
    /// charset and sql_mode (NO_BACKSLASH_ESCAPES included); NULL for nil.
    private func literal(_ value: String?) -> String {
        guard let value else { return "NULL" }
        let bytes = Array(value.utf8)
        var buffer = [CChar](repeating: 0, count: bytes.count * 2 + 1)
        let length = bytes.withUnsafeBufferPointer { source in
            source.withMemoryRebound(to: CChar.self) { mysql_real_escape_string(conn, &buffer, $0.baseAddress, UInt($0.count)) }
        }
        let escaped = buffer.prefix(Int(length)).map { UInt8(bitPattern: $0) }
        return "'" + String(decoding: escaped, as: UTF8.self) + "'"
    }
}

/// Cancels by sending KILL QUERY from a second connection, as the mysql client does.
private final class MySQLCanceller: Sendable {
    private let target: MySQLTarget
    private let threadID: UInt
    /// Whether a query is running, and whether it was asked to stop.
    private let state = Mutex<(running: Bool, requested: Bool)>((false, false))

    init(target: MySQLTarget, threadID: UInt) {
        self.target = target
        self.threadID = threadID
    }

    func begin() { state.withLock { $0 = (true, false) } }

    /// Ends a run; true when it was canceled.
    func end() -> Bool {
        state.withLock { state in
            defer { state = (false, false) }
            return state.requested
        }
    }

    func cancel() {
        let running = state.withLock { state in
            if state.running { state.requested = true }
            return state.running
        }
        guard running else { return }
        let target = target
        let id = threadID
        DispatchQueue.global(qos: .userInitiated).async {
            guard let conn = try? target.open() else { return }
            defer { mysql_close(conn) }
            let sql = "KILL QUERY \(id)"
            _ = mysql_real_query(conn, sql, UInt(sql.utf8.count))
        }
    }
}

extension MySQLSession {
    public func schemaGraph() async throws -> SchemaGraph {
        let system = "('mysql','information_schema','performance_schema','sys')"
        let columns = try single("""
            select c.table_schema, c.table_name, t.table_type = 'VIEW', c.column_name, c.column_type,
                   c.is_nullable = 'YES', c.column_key = 'PRI'
            from information_schema.columns c
            join information_schema.tables t on t.table_schema = c.table_schema and t.table_name = c.table_name
            where c.table_schema not in \(system)
            order by c.table_schema = database() desc, c.table_schema, c.table_name, c.ordinal_position
            """).rows
        let links = try single("""
            select constraint_name, concat(table_schema, '.', table_name),
                   concat(referenced_table_schema, '.', referenced_table_name),
                   group_concat(column_name order by ordinal_position separator 0x1F),
                   group_concat(referenced_column_name order by ordinal_position separator 0x1F)
            from information_schema.key_column_usage
            where referenced_table_name is not null and table_schema not in \(system)
            group by table_schema, table_name, constraint_name, referenced_table_schema, referenced_table_name
            order by 1
            """).rows
        return SchemaGraph.build(columns: columns, links: links)
    }
}

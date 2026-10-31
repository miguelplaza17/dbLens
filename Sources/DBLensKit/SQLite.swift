import Foundation
import SQLite3

// MARK: - Location

/// Where a SQLite file is, read from what people paste: sqlite:///abs/x.db,
/// sqlite://~/x.db, sqlite:relative.db, file:/x.db, a plain path, or an .env line.
public struct SQLiteLocation: Sendable, Hashable {
    /// The cleaned text that was pasted.
    public let raw: String
    /// The absolute path, with ~ expanded.
    public let path: String

    /// The file name, "app.db", for the sidebar.
    public var suggestedName: String { (path as NSString).lastPathComponent }

    private static let extensions: Set<String> = ["db", "db3", "sqlite", "sqlite3"]
    /// Longest first, so "sqlite3://" is not read as "sqlite3:" plus "//".
    private static let schemes = ["sqlite3://", "sqlite://", "sqlite3:", "sqlite:", "file://", "file:"]

    public init(_ text: String) throws {
        let cleaned = Self.stripEnvName(ConnectionURL.clean(text))
        guard !cleaned.isEmpty else { throw Self.notSQLite }
        raw = cleaned
        path = try Self.resolve(cleaned)
    }

    private static var notSQLite: DatabaseError {
        DatabaseError(message: String(localized: "That doesn't look like a SQLite file. Use a path ending in .db, .sqlite or .sqlite3, or sqlite:///path/to/file.db."))
    }

    /// `DB_PATH=/x/app.db` and `DB=~/app.db`: ConnectionURL.clean only strips the name before a URL.
    private static func stripEnvName(_ text: String) -> String {
        guard let match = text.range(of: "^[A-Za-z_][A-Za-z0-9_]*\\s*=\\s*", options: .regularExpression) else { return text }
        let rest = text[match.upperBound...].trimmingCharacters(in: CharacterSet(charactersIn: "\"' \t"))
        guard rest.hasPrefix("/") || rest.hasPrefix("~") || rest.hasPrefix(".") else { return text }
        return rest
    }

    private static func resolve(_ text: String) throws -> String {
        let lower = text.lowercased()
        guard let scheme = schemes.first(where: { lower.hasPrefix($0) }) else {
            guard let first = text.first, "/~.".contains(first),
                  extensions.contains((text as NSString).pathExtension.lowercased()) else { throw notSQLite }
            return absolute(text)
        }
        var rest = String(text.dropFirst(scheme.count))
        if let cut = rest.firstIndex(where: { $0 == "?" || $0 == "#" }) { rest = String(rest[..<cut]) }
        if scheme.hasPrefix("file") {
            rest = rest.removingPercentEncoding ?? rest
            // file://localhost/path: the host is dropped.
            if scheme == "file://", !rest.hasPrefix("/"), let slash = rest.firstIndex(of: "/"),
               ["", "localhost"].contains(rest[..<slash].lowercased()) {
                rest = String(rest[slash...])
            }
        }
        guard !rest.isEmpty, rest != "/" else { throw notSQLite }
        return absolute(rest)
    }

    /// ~ expanded; a relative path is relative to the home folder (an app has no useful working directory).
    private static func absolute(_ text: String) -> String {
        var path = text
        if path == "~" || path.hasPrefix("~/") { path = NSHomeDirectory() + path.dropFirst() }
        else if !path.hasPrefix("/") { path = NSHomeDirectory() + "/" + path }
        return URL(fileURLWithPath: path).standardized.path
    }
}

// MARK: - Session

/// SQLITE_TRANSIENT: SQLite copies the bound text before bind returns.
private let sqliteTransient = unsafeBitCast(-1, to: sqlite3_destructor_type.self)

/// One open SQLite file. SQLite calls block, so each runs on the session's own serial queue.
public actor SQLiteSession: DatabaseSession {
    public nonisolated var engine: Engine { .sqlite }
    public nonisolated var tunnel: SSHTunnel? { nil }
    public nonisolated func quote(_ identifier: String) -> String {
        "\"" + identifier.replacingOccurrences(of: "\"", with: "\"\"") + "\""
    }

    private let queue = DispatchSerialQueue(label: "dbLens.sqlite")
    public nonisolated var unownedExecutor: UnownedSerialExecutor { queue.asUnownedSerialExecutor() }

    /// Only used on `queue`, except sqlite3_interrupt, which SQLite allows from any thread.
    nonisolated(unsafe) private let db: OpaquePointer
    public nonisolated let location: SQLiteLocation
    public nonisolated let serverVersion: String
    /// True when the file couldn't be opened for writing.
    public nonisolated let isReadOnly: Bool

    /// Opens an existing file, or throws SQLite's own message.
    public static func open(_ location: SQLiteLocation) async throws -> SQLiteSession {
        try await Task.detached { try SQLiteSession(location: location) }.value
    }

    private init(location: SQLiteLocation) throws {
        guard FileManager.default.fileExists(atPath: location.path) else {
            throw DatabaseError(message: String(localized: "No SQLite file at \(location.path)"))
        }
        var handle: OpaquePointer?
        var readOnly = false
        var rc = sqlite3_open_v2(location.path, &handle, SQLITE_OPEN_READWRITE | SQLITE_OPEN_FULLMUTEX, nil)
        if rc != SQLITE_OK {
            sqlite3_close_v2(handle)
            handle = nil
            readOnly = true
            rc = sqlite3_open_v2(location.path, &handle, SQLITE_OPEN_READONLY | SQLITE_OPEN_FULLMUTEX, nil)
        }
        guard rc == SQLITE_OK, let handle else {
            let message = handle.map { String(cString: sqlite3_errmsg($0)) }
                ?? String(localized: "Couldn't open the file.")
            sqlite3_close_v2(handle)
            throw DatabaseError(message: message)
        }
        // SQLite opens a write-protected file read-only without an error.
        if sqlite3_db_readonly(handle, "main") == 1 { readOnly = true }
        sqlite3_busy_timeout(handle, 5_000)
        // A file that isn't a database only fails on first use; fail now.
        let ready = sqlite3_exec(handle, "pragma foreign_keys = on; select 1 from sqlite_schema limit 1", nil, nil, nil)
        guard ready == SQLITE_OK else {
            let message = String(cString: sqlite3_errmsg(handle))
            sqlite3_close_v2(handle)
            throw DatabaseError(message: message)
        }
        db = handle
        self.location = location
        isReadOnly = readOnly
        serverVersion = String(cString: sqlite3_libversion())
    }

    deinit { sqlite3_close_v2(db) }

    /// Stops the running query. Safe to call from any thread.
    public nonisolated func cancel() { sqlite3_interrupt(db) }

    // MARK: Running SQL

    /// Runs one or more statements, one after another.
    public func execute(_ sql: String) throws -> Execution {
        let clock = ContinuousClock()
        let start = clock.now
        var results: [QueryResult] = []
        try sql.withCString { text in
            var cursor: UnsafePointer<CChar>? = text
            while let current = cursor, current.pointee != 0 {
                var statement: OpaquePointer?
                var tail: UnsafePointer<CChar>?
                guard sqlite3_prepare_v2(db, current, -1, &statement, &tail) == SQLITE_OK else { throw lastError() }
                cursor = tail
                // Whitespace or a comment alone prepares to nothing.
                guard let statement else { continue }
                defer { sqlite3_finalize(statement) }
                results.append(try read(statement))
            }
        }
        return Execution(results: results, duration: clock.now - start)
    }

    /// One statement, run only when SQLite says it changes nothing
    /// (sqlite3_stmt_readonly), with query_only on as a second lock.
    public func readOnly(_ sql: String) throws -> Execution {
        let clock = ContinuousClock()
        let start = clock.now
        var statement: OpaquePointer?
        var tail: UnsafePointer<CChar>?
        let result: QueryResult = try sql.withCString { text in
            guard sqlite3_prepare_v2(db, text, -1, &statement, &tail) == SQLITE_OK else { throw lastError() }
            guard let statement else { throw DatabaseError(message: String(localized: "There is no statement to run.")) }
            defer { sqlite3_finalize(statement) }
            // Whatever follows must prepare to nothing (spaces, semicolons, comments).
            var next: OpaquePointer?
            if let tail { sqlite3_prepare_v2(db, tail, -1, &next, nil) }
            if let next {
                sqlite3_finalize(next)
                throw DatabaseError(message: String(localized: "Send one statement at a time."))
            }
            // sqlite3_stmt_readonly passes these too: they change the connection, not the file.
            let keyword = Self.firstKeyword(sql)
            guard !["ATTACH", "DETACH", "PRAGMA", "BEGIN", "COMMIT", "END", "ROLLBACK", "SAVEPOINT", "RELEASE", "VACUUM"].contains(keyword),
                  sqlite3_stmt_readonly(statement) != 0 else {
                throw DatabaseError(message: String(localized: "This statement would change the database; read-only queries can't."))
            }
            sqlite3_exec(db, "pragma query_only = on", nil, nil, nil)
            defer { sqlite3_exec(db, "pragma query_only = off", nil, nil, nil) }
            return try read(statement)
        }
        return Execution(results: [result], duration: clock.now - start)
    }

    private func read(_ statement: OpaquePointer) throws -> QueryResult {
        let count = Int(sqlite3_column_count(statement))
        let columns = (0..<Int32(count)).map { index in
            Column(name: sqlite3_column_name(statement, index).map(String.init(cString:)) ?? "",
                   type: sqlite3_column_decltype(statement, index).map(String.init(cString:)) ?? "")
        }
        var rows: [[String?]] = []
        var total = 0
        while true {
            let rc = sqlite3_step(statement)
            if rc == SQLITE_DONE { break }
            guard rc == SQLITE_ROW else { throw lastError() }
            total += 1
            if rows.count < QueryResult.rowLimit {
                rows.append((0..<Int32(count)).map { Self.value(statement, $0) })
            }
        }
        let command = count > 0 ? "SELECT \(total)" : commandTag(for: statement)
        return QueryResult(columns: columns, rows: rows, command: command,
                           truncated: total > rows.count, totalRows: total)
    }

    private func commandTag(for statement: OpaquePointer) -> String {
        let keyword = Self.firstKeyword(sqlite3_sql(statement).map(String.init(cString:)) ?? "")
        guard ["INSERT", "UPDATE", "DELETE"].contains(keyword) else { return keyword }
        return "\(keyword) \(sqlite3_changes(db))"
    }

    /// The statement's first word, uppercased, after any whitespace, stray semicolons and comments.
    static func firstKeyword(_ sql: String) -> String {
        var rest = Substring(sql)
        while true {
            rest = rest.drop(while: { $0.isWhitespace || $0 == ";" })
            if rest.hasPrefix("--") {
                rest = rest.drop(while: { $0 != "\n" })
            } else if rest.hasPrefix("/*") {
                guard let end = rest.range(of: "*/") else { return "" }
                rest = rest[end.upperBound...]
            } else {
                break
            }
        }
        return String(rest.prefix(while: { $0.isLetter })).uppercased()
    }

    private static let blobBytesShown = 64

    private static func value(_ statement: OpaquePointer, _ column: Int32) -> String? {
        switch sqlite3_column_type(statement, column) {
        case SQLITE_NULL:
            return nil
        case SQLITE_BLOB:
            let length = Int(sqlite3_column_bytes(statement, column))
            guard length > 0, let bytes = sqlite3_column_blob(statement, column) else { return "0x" }
            let shown = UnsafeBufferPointer(start: bytes.assumingMemoryBound(to: UInt8.self), count: min(length, blobBytesShown))
            return "0x" + shown.map { String(format: "%02x", $0) }.joined() + (length > blobBytesShown ? "…" : "")
        default:
            // Read by length, so a NUL inside the text doesn't cut it.
            guard let text = sqlite3_column_text(statement, column) else { return nil }
            let length = Int(sqlite3_column_bytes(statement, column))
            return String(decoding: UnsafeBufferPointer(start: text, count: length), as: UTF8.self)
        }
    }

    private func lastError() -> DatabaseError {
        DatabaseError(message: String(cString: sqlite3_errmsg(db)))
    }

    private func single(_ sql: String) throws -> QueryResult {
        guard let main = try execute(sql).main else { throw DatabaseError(message: String(localized: "No result.")) }
        return main
    }

    // MARK: Browsing

    public func relations() throws -> [Relation] {
        try single("""
            select name, type from sqlite_schema
            where type in ('table', 'view') and name not like 'sqlite\\_%' escape '\\'
            order by name
            """).rows.map { row in
            Relation(schema: "main", name: row[0] ?? "", kind: row[1] == "view" ? .view : .table, estimate: -1)
        }
    }

    /// Column names and declared types, in table order, plus the primary key in key order.
    private func tableInfo(_ relation: Relation) throws -> (types: [String: String], primaryKey: [String], columns: [String]) {
        let rows = try single("pragma table_info(\(quote(relation.name)))").rows
        // cid, name, type, notnull, dflt_value, pk
        var types: [String: String] = [:]
        var keyed: [(order: Int, name: String)] = []
        var columns: [String] = []
        for row in rows {
            let name = row[1] ?? ""
            columns.append(name)
            types[name] = row[2] ?? ""
            if let order = Int(row[5] ?? ""), order > 0 { keyed.append((order, name)) }
        }
        return (types, keyed.sorted { $0.order < $1.order }.map(\.name), columns)
    }

    /// One page of a table's rows. Sorted by the primary key, or by rowid when there is none
    /// (a WITHOUT ROWID table always has a primary key, so it never needs the rowid).
    public func page(of relation: Relation, limit: Int, offset: Int, search: String = "", sort: ColumnSort? = nil) throws -> QueryResult {
        let info = try tableInfo(relation)
        let isTable = relation.kind == .table
        let useRowid = isTable && info.primaryKey.isEmpty
        let keys = info.primaryKey.isEmpty ? (useRowid ? ["rowid"] : []) : info.primaryKey.map(quote)
        let terms = (sort.map { ["\(quote($0.column)) \($0.direction) nulls last"] } ?? []) + keys
        let order = terms.isEmpty ? "" : " order by " + terms.joined(separator: ", ")
        var result = try single("select \(useRowid ? "rowid, " : "")* from \(qualified(relation))\(filter(search, columns: info.columns))\(order) limit \(limit) offset \(offset)")
        if useRowid {
            let keys = result.rows.map { [$0[0]] }
            result.columns.removeFirst()
            result.rows = result.rows.map { Array($0.dropFirst()) }
            result.keys = RowKeys(columns: ["rowid"], kind: .rowid, values: keys)
        } else if isTable {
            let indexes = info.primaryKey.compactMap { name in result.columns.firstIndex { $0.name == name } }
            if indexes.count == info.primaryKey.count {
                result.keys = RowKeys(columns: info.primaryKey, kind: .primaryKey,
                                      values: result.rows.map { row in indexes.map { row[$0] } })
            }
        }
        result.columns = result.columns.map { Column(name: $0.name, type: info.types[$0.name] ?? $0.type) }
        return result
    }

    /// Exact count; SQLite keeps no estimate.
    public func count(of relation: Relation, search: String = "") throws -> Int? {
        let where_ = search.isEmpty ? "" : filter(search, columns: try tableInfo(relation).columns)
        return Int(try single("select count(*) from \(qualified(relation))\(where_)").rows.first?.first.flatMap { $0 } ?? "")
    }

    /// Every column joined into one text and matched with LIKE (case-insensitive for ASCII).
    private func filter(_ search: String, columns: [String]) -> String {
        guard !search.isEmpty, !columns.isEmpty else { return "" }
        let joined = columns.map { "coalesce(\(quote($0)), '')" }.joined(separator: " || ' ' || ")
        let pattern = "'" + likePattern(search).replacingOccurrences(of: "'", with: "''") + "'"
        return " where (\(joined)) like \(pattern) escape '\\'"
    }

    // MARK: Editing

    public func update(_ relation: Relation, keys: RowKeys, row: Int, changes: [(String, String?)]) throws -> EditOutcome {
        guard !changes.isEmpty else { return EditOutcome(sql: "", command: "UPDATE 0", affected: 0) }
        try requireWritable()
        let set = changes.map { "\(quote($0.0)) = ?" }.joined(separator: ", ")
        let (filter, keyParams) = whereClause(keys, row: row)
        return try edit("update \(qualified(relation)) set \(set) where \(filter)",
                        params: changes.map(\.1) + keyParams, verb: "UPDATE", expectOne: true)
    }

    /// Adds a row. Columns left out get their default.
    public func insert(_ relation: Relation, values: [(String, String?)]) throws -> EditOutcome {
        try requireWritable()
        let sql = values.isEmpty
            ? "insert into \(qualified(relation)) default values"
            : "insert into \(qualified(relation)) (\(values.map { quote($0.0) }.joined(separator: ", "))) values (\(values.map { _ in "?" }.joined(separator: ", ")))"
        return try edit(sql, params: values.map(\.1), verb: "INSERT", expectOne: false)
    }

    public func delete(_ relation: Relation, keys: RowKeys, row: Int) throws -> EditOutcome {
        try requireWritable()
        let (filter, params) = whereClause(keys, row: row)
        return try edit("delete from \(qualified(relation)) where \(filter)", params: params, verb: "DELETE", expectOne: true)
    }

    private func requireWritable() throws {
        if isReadOnly { throw DatabaseError(message: String(localized: "This file is read-only.")) }
    }

    /// `"col" = ?` per key column; a NULL key value compares with `is null` and binds nothing.
    private func whereClause(_ keys: RowKeys, row: Int) -> (String, [String?]) {
        var parts: [String] = []
        var params: [String?] = []
        for (name, value) in zip(keys.columns, keys.values[row]) {
            let column = keys.kind == .rowid ? name : quote(name)
            if let value {
                parts.append("\(column) = ?")
                params.append(value)
            } else {
                parts.append("\(column) is null")
            }
        }
        return (parts.joined(separator: " and "), params)
    }

    /// Runs one statement with bound text. An edit meant for exactly one row is
    /// rolled back if it would touch more, and refused if it touches none.
    private func edit(_ sql: String, params: [String?], verb: String, expectOne: Bool) throws -> EditOutcome {
        try exec("begin")
        do {
            let affected = try runBound(sql, params: params)
            if expectOne {
                guard affected <= 1 else {
                    throw DatabaseError(message: String(localized: "The change would have affected \(affected) rows and was rolled back."))
                }
                guard affected == 1 else {
                    throw DatabaseError(message: String(localized: "The row changed or was deleted since it was loaded. Reload the table and try again."))
                }
            }
            try exec("commit")
            return EditOutcome(sql: sql, command: "\(verb) \(affected)", affected: affected)
        } catch {
            _ = try? exec("rollback")
            throw error
        }
    }

    private func exec(_ sql: String) throws {
        guard sqlite3_exec(db, sql, nil, nil, nil) == SQLITE_OK else { throw lastError() }
    }

    /// Prepares, binds `?` parameters as text (nil is NULL), steps to the end, and returns the rows changed.
    private func runBound(_ sql: String, params: [String?]) throws -> Int {
        var statement: OpaquePointer?
        guard sqlite3_prepare_v2(db, sql, -1, &statement, nil) == SQLITE_OK else { throw lastError() }
        defer { sqlite3_finalize(statement) }
        for (index, value) in params.enumerated() {
            let position = Int32(index + 1)
            let rc = value.map { sqlite3_bind_text(statement, position, $0, -1, sqliteTransient) }
                ?? sqlite3_bind_null(statement, position)
            guard rc == SQLITE_OK else { throw lastError() }
        }
        var rc = sqlite3_step(statement)
        while rc == SQLITE_ROW { rc = sqlite3_step(statement) }
        guard rc == SQLITE_DONE else { throw lastError() }
        return Int(sqlite3_changes(db))
    }
}

extension SQLiteSession {
    public func schemaGraph() async throws -> SchemaGraph {
        let names = try single("""
            select name, type = 'view' from sqlite_schema
            where type in ('table','view') and name not like 'sqlite_%' order by name
            """).rows
        var columns: [[String?]] = []
        var links: [[String?]] = []
        for row in names {
            guard let name = row[0] else { continue }
            let quoted = quote(name)
            for column in try single("pragma table_info(\(quoted))").rows {
                // cid, name, type, notnull, dflt_value, pk
                columns.append(["main", name, row[1], column[1], column[2],
                                column[3] == "1" ? "0" : "1", (Int(column[5] ?? "0") ?? 0) > 0 ? "1" : "0"])
            }
            // id, seq, table, from, to, …: one entry per column of each foreign key
            var grouped: [String: (to: String, from: [String], toColumns: [String])] = [:]
            for key in try single("pragma foreign_key_list(\(quoted))").rows {
                let id = key[0] ?? ""
                var entry = grouped[id] ?? (key[2] ?? "", [], [])
                entry.from.append(key[3] ?? "")
                entry.toColumns.append(key[4] ?? "")
                grouped[id] = entry
            }
            let separator = String(UnicodeScalar(31))
            for (id, entry) in grouped.sorted(by: { $0.key < $1.key }) {
                links.append(["fk_\(name)_\(id)", "main.\(name)", "main.\(entry.to)",
                              entry.from.joined(separator: separator), entry.toColumns.joined(separator: separator)])
            }
        }
        var graph = SchemaGraph.build(columns: columns, links: links)
        // A foreign key without target columns points at the target's primary key.
        graph.links = graph.links.map { link in
            guard link.toColumns.allSatisfy(\.isEmpty),
                  let target = graph.tables.first(where: { $0.id == link.to }) else { return link }
            return SchemaGraph.Link(name: link.name, from: link.from, fromColumns: link.fromColumns, to: link.to,
                                    toColumns: target.columns.filter(\.isPrimaryKey).map(\.name))
        }
        return graph
    }
}

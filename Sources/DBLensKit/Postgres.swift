import CLibPQ
import Foundation

/// One open connection. libpq blocks, so every call runs on the session's own
/// serial queue, never on the main thread or the shared pool.
public actor PostgresSession: DatabaseSession {
    /// Results bigger than this are cut, so the grid stays fast.
    public static var rowLimit: Int { QueryResult.rowLimit }
    public nonisolated var engine: Engine { .postgres }
    public nonisolated func quote(_ identifier: String) -> String { quoteIdent(identifier) }

    private let queue = DispatchSerialQueue(label: "dbLens.libpq")
    public nonisolated var unownedExecutor: UnownedSerialExecutor { queue.asUnownedSerialExecutor() }

    /// Only touched on `queue`, and by deinit once nothing else can.
    nonisolated(unsafe) private let conn: OpaquePointer
    private let canceller: Canceller
    public nonisolated let url: ConnectionURL
    public nonisolated let serverVersion: String

    /// The SSH tunnel this connection goes through, if any. It closes with the session.
    public nonisolated let tunnel: SSHTunnel?

    /// Connects, or throws Postgres' own message.
    public static func connect(_ url: ConnectionURL) async throws -> PostgresSession {
        try await Task.detached { try PostgresSession(url: url, tunnel: nil) }.value
    }

    /// Opens an SSH tunnel to the database's host and port (as the SSH server
    /// sees them), then connects through it.
    public static func connect(_ url: ConnectionURL, via ssh: SSHTunnel.Config) async throws -> PostgresSession {
        let tunnel = try await SSHTunnel.open(ssh, to: url.host, port: url.port ?? 5432)
        do {
            return try await Task.detached { try PostgresSession(url: url, tunnel: tunnel) }.value
        } catch {
            tunnel.close()
            throw error
        }
    }

    /// A Mac app starts in the "C" locale, so libpq's translated messages come
    /// out as ASCII ("n~ao"). A UTF-8 character type keeps the accents.
    private static let utf8Messages: Void = { setlocale(LC_CTYPE, "UTF-8") }()

    private init(url: ConnectionURL, tunnel: SSHTunnel?) throws {
        _ = Self.utf8Messages
        let conninfo = tunnel.map { url.forLibpq(throughLocalPort: $0.localPort) } ?? url.forLibpq
        guard let conn = PQconnectdb(conninfo) else { throw PostgresError(message: String(localized: "Out of memory while connecting.")) }
        guard PQstatus(conn) == CONNECTION_OK else {
            let message = Self.clean(PQerrorMessage(conn))
            PQfinish(conn)
            throw PostgresError(message: message.isEmpty ? String(localized: "Couldn't connect.") : message)
        }
        self.conn = conn
        self.url = url
        self.tunnel = tunnel
        canceller = Canceller(PQgetCancel(conn))
        let version = Int(PQserverVersion(conn))
        serverVersion = "\(version / 10_000).\(version % 10_000)"
    }

    deinit {
        PQfinish(conn)
        tunnel?.close()
    }

    /// Runs one or more statements, the way psql does with -c.
    public func execute(_ sql: String) throws -> Execution {
        let clock = ContinuousClock()
        let start = clock.now
        guard PQsendQuery(conn, sql) == 1 else { throw PostgresError(message: Self.clean(PQerrorMessage(conn))) }
        var results: [QueryResult] = []
        var failure: PostgresError?
        while let res = PQgetResult(conn) {
            defer { PQclear(res) }
            switch PQresultStatus(res) {
            case PGRES_TUPLES_OK, PGRES_COMMAND_OK, PGRES_EMPTY_QUERY, PGRES_SINGLE_TUPLE:
                results.append(Self.read(res))
            case PGRES_FATAL_ERROR, PGRES_BAD_RESPONSE, PGRES_NONFATAL_ERROR:
                failure = failure ?? PostgresError(message: Self.clean(PQresultErrorMessage(res)))
            default:
                // COPY is out of scope; libpq needs the copy ended before the next query.
                failure = failure ?? PostgresError(message: String(localized: "COPY is not supported here. Use psql."))
                PQputCopyEnd(conn, "not supported")
            }
        }
        if let failure { throw failure }
        return Execution(results: results, duration: clock.now - start)
    }

    /// Stops the running query. Safe to call from any thread.
    public nonisolated func cancel() { canceller.cancel() }

    public func relations() throws -> [Relation] {
        let sql = """
            select n.nspname, c.relname, c.relkind, c.reltuples
            from pg_class c join pg_namespace n on n.oid = c.relnamespace
            where c.relkind in ('r','p','v','m','f')
              and n.nspname not in ('pg_catalog','information_schema')
              and n.nspname not like 'pg_toast%' and n.nspname not like 'pg_temp%'
            order by n.nspname = 'public' desc, n.nspname, c.relname
            """
        return try single(sql).rows.map { row in
            let kind: Relation.Kind = switch row[2] {
            case "v": .view
            case "m": .materialized
            case "f": .foreign
            case "p": .partitioned
            default: .table
            }
            return Relation(schema: row[0] ?? "", name: row[1] ?? "", kind: kind, estimate: Double(row[3] ?? "") ?? -1)
        }
    }

    /// One page of a table's rows, with the column types and, for tables, the
    /// row keys. Sorted by the primary key when there is one, so pages and
    /// edited rows stay in place.
    public func page(of relation: Relation, limit: Int, offset: Int, search: String = "", sort: ColumnSort? = nil) throws -> QueryResult {
        let primaryKey = try primaryKey(of: relation)
        let usesCtid = primaryKey.isEmpty && relation.kind == .table
        let keys = primaryKey.isEmpty ? (usesCtid ? ["t.ctid"] : []) : primaryKey.map(quoteIdent)
        let terms = (sort.map { ["\(quoteIdent($0.column)) \($0.direction) nulls last"] } ?? []) + keys
        let order = terms.isEmpty ? "" : " order by " + terms.joined(separator: ", ")
        // Without a primary key, a row is found by ctid, checked with xmin: if another
        // session changed or vacuumed it since, the ctid may point to another row.
        var result = try single("select \(usesCtid ? "ctid::text, xmin::text, " : "")* from \(relation.qualified) as t\(filter(search))\(order) limit \(limit) offset \(offset)")
        if usesCtid {
            let keys = result.rows.map { Array($0.prefix(2)) }
            result.columns.removeFirst(2)
            result.rows = result.rows.map { Array($0.dropFirst(2)) }
            result.keys = RowKeys(columns: ["ctid", "xmin"], kind: .ctid, values: keys)
        } else if !primaryKey.isEmpty {
            let indexes = primaryKey.compactMap { name in result.columns.firstIndex { $0.name == name } }
            if indexes.count == primaryKey.count {
                result.keys = RowKeys(columns: primaryKey, kind: .primaryKey,
                                      values: result.rows.map { row in indexes.map { row[$0] } })
            }
        }
        let types = try single("""
            select a.attname, format_type(a.atttypid, a.atttypmod)
            from pg_attribute a
            where a.attrelid = \(literal(relation.qualified))::regclass
              and a.attnum > 0 and not a.attisdropped
            """).rows
        let byName = Dictionary(types.map { ($0[0] ?? "", $0[1] ?? "") }, uniquingKeysWith: { a, _ in a })
        result.columns = result.columns.map { Column(name: $0.name, type: byName[$0.name] ?? "") }
        return result
    }

    /// The primary-key columns of a table, in key order. Empty for views and keyless tables.
    public func primaryKey(of relation: Relation) throws -> [String] {
        guard relation.kind == .table || relation.kind == .partitioned else { return [] }
        return try single("""
            select a.attname
            from pg_index i
            join lateral unnest(i.indkey) with ordinality as k(attnum, n) on true
            join pg_attribute a on a.attrelid = i.indrelid and a.attnum = k.attnum
            where i.indrelid = \(literal(relation.qualified))::regclass and i.indisprimary
            order by k.n
            """).rows.compactMap { $0.first ?? nil }
    }

    // MARK: Editing

    /// Changes one row. `changes` maps column names to new text (nil is NULL);
    /// Postgres casts the text to each column's type.
    public func update(_ relation: Relation, keys: RowKeys, row: Int, changes: [(String, String?)]) throws -> EditOutcome {
        guard !changes.isEmpty else { return EditOutcome(sql: "", command: "UPDATE 0", affected: 0) }
        var params = changes.map(\.1)
        let set = changes.enumerated().map { "\(quoteIdent($0.element.0)) = $\($0.offset + 1)" }.joined(separator: ", ")
        let (filter, keyParams) = whereClause(keys, row: row, after: params.count)
        params += keyParams
        let sql = "update \(Self.only(relation))\(relation.qualified) set \(set) where \(filter)"
        return try edit(sql, params: params, expectOne: true)
    }

    /// Adds a row. Columns left out get their default.
    public func insert(_ relation: Relation, values: [(String, String?)]) throws -> EditOutcome {
        let sql = values.isEmpty
            ? "insert into \(relation.qualified) default values"
            : "insert into \(relation.qualified) (\(values.map { quoteIdent($0.0) }.joined(separator: ", "))) values (\(values.indices.map { "$\($0 + 1)" }.joined(separator: ", ")))"
        return try edit(sql, params: values.map(\.1), expectOne: false)
    }

    public func delete(_ relation: Relation, keys: RowKeys, row: Int) throws -> EditOutcome {
        let (filter, params) = whereClause(keys, row: row, after: 0)
        return try edit("delete from \(Self.only(relation))\(relation.qualified) where \(filter)", params: params, expectOne: true)
    }

    /// "only " for a plain table, so an edit never reaches a table that inherits from it.
    private static func only(_ relation: Relation) -> String { relation.kind == .table ? "only " : "" }

    private func whereClause(_ keys: RowKeys, row: Int, after count: Int) -> (String, [String?]) {
        let values = keys.values[row]
        let parts = keys.columns.enumerated().map { index, name in
            guard keys.usesCtid else { return "\(quoteIdent(name)) = $\(count + index + 1)" }
            return name == "ctid" ? "ctid = $\(count + index + 1)::tid" : "xmin::text = $\(count + index + 1)"
        }
        return (parts.joined(separator: " and "), values)
    }

    /// Runs one statement with text parameters. An edit meant for exactly one
    /// row is rolled back if it would touch more, and refused if it touches none.
    private func edit(_ sql: String, params: [String?], expectOne: Bool) throws -> EditOutcome {
        if expectOne { try run("begin", params: []) }
        do {
            let result = try run(sql, params: params)
            let affected = Int(result.command.split(separator: " ").last ?? "") ?? 0
            if expectOne {
                guard affected <= 1 else {
                    _ = try? run("rollback", params: [])
                    throw PostgresError(message: String(localized: "The change would have affected \(affected) rows and was rolled back."))
                }
                guard affected == 1 else {
                    _ = try? run("rollback", params: [])
                    throw PostgresError(message: String(localized: "The row changed or was deleted since it was loaded. Reload the table and try again."))
                }
                try run("commit", params: [])
            }
            return EditOutcome(sql: sql, command: result.command, affected: affected)
        } catch {
            if expectOne { _ = try? run("rollback", params: []) }
            throw error
        }
    }

    /// One statement with $1, $2… parameters sent as text (nil is NULL).
    @discardableResult
    public func run(_ sql: String, params: [String?]) throws -> QueryResult {
        let values = params.map { $0.flatMap { strdup($0) } }
        defer { values.forEach { free($0) } }
        let res = values.withUnsafeBufferPointer { buffer in
            buffer.baseAddress?.withMemoryRebound(to: UnsafePointer<CChar>?.self, capacity: buffer.count) {
                PQexecParams(conn, sql, Int32(buffer.count), nil, $0, nil, nil, 0)
            } ?? PQexecParams(conn, sql, 0, nil, nil, nil, nil, 0)
        }
        guard let res else { throw PostgresError(message: Self.clean(PQerrorMessage(conn))) }
        defer { PQclear(res) }
        switch PQresultStatus(res) {
        case PGRES_TUPLES_OK, PGRES_COMMAND_OK, PGRES_EMPTY_QUERY:
            return Self.read(res)
        default:
            throw PostgresError(message: Self.clean(PQresultErrorMessage(res)))
        }
    }

    private func literal(_ text: String) -> String {
        "'" + text.replacingOccurrences(of: "'", with: "''") + "'"
    }

    /// Exact row count, or nil when the table is big enough that counting is slow.
    public func count(of relation: Relation, search: String = "") throws -> Int? {
        if relation.estimate > 500_000 { return nil }
        return Int(try single("select count(*) from \(relation.qualified) as t\(filter(search))").rows.first?.first.flatMap { $0 } ?? "")
    }

    /// Matches the whole row as text, so every column is searched at once.
    private func filter(_ search: String) -> String {
        search.isEmpty ? "" : " where t::text ilike \(literal(likePattern(search)))"
    }

    private func single(_ sql: String) throws -> QueryResult {
        guard let main = try execute(sql).main else { throw PostgresError(message: String(localized: "No result.")) }
        return main
    }

    private static func read(_ res: OpaquePointer) -> QueryResult {
        let fields = Int(PQnfields(res))
        let total = Int(PQntuples(res))
        let kept = min(total, rowLimit)
        let columns = (0..<fields).map { Column(name: String(cString: PQfname(res, Int32($0)))) }
        var rows: [[String?]] = []
        rows.reserveCapacity(kept)
        for r in 0..<Int32(kept) {
            rows.append((0..<Int32(fields)).map { c in
                PQgetisnull(res, r, c) == 1 ? nil : String(cString: PQgetvalue(res, r, c))
            })
        }
        return QueryResult(columns: columns, rows: rows, command: String(cString: PQcmdStatus(res)),
                           truncated: total > kept, totalRows: total)
    }

    private static func clean(_ message: UnsafePointer<CChar>?) -> String {
        guard let message else { return "" }
        return String(cString: message).trimmingCharacters(in: .whitespacesAndNewlines)
    }
}

/// PQcancel is thread safe and meant to be called while the query runs.
private final class Canceller: @unchecked Sendable {
    private let handle: OpaquePointer?
    init(_ handle: OpaquePointer?) { self.handle = handle }
    deinit { if let handle { PQfreeCancel(handle) } }

    func cancel() {
        guard let handle else { return }
        var buffer = [CChar](repeating: 0, count: 256)
        _ = PQcancel(handle, &buffer, Int32(buffer.count))
    }
}

extension PostgresSession {
    public func schemaGraph() async throws -> SchemaGraph {
        let columns = try single("""
            select n.nspname, c.relname, c.relkind in ('v','m'), a.attname,
                   format_type(a.atttypid, a.atttypmod), not a.attnotnull,
                   coalesce(a.attnum = any(i.indkey::int2[]), false)
            from pg_class c
            join pg_namespace n on n.oid = c.relnamespace
            join pg_attribute a on a.attrelid = c.oid and a.attnum > 0 and not a.attisdropped
            left join pg_index i on i.indrelid = c.oid and i.indisprimary
            where c.relkind in ('r','p','v','m','f') and not c.relispartition
              and n.nspname not in ('pg_catalog','information_schema')
              and n.nspname not like 'pg_toast%' and n.nspname not like 'pg_temp%'
            order by n.nspname = 'public' desc, n.nspname, c.relname, a.attnum
            """).rows
        let links = try single("""
            select con.conname, sn.nspname || '.' || sc.relname, tn.nspname || '.' || tc.relname,
                   (select string_agg(a.attname, chr(31) order by k.n) from unnest(con.conkey) with ordinality k(attnum, n)
                      join pg_attribute a on a.attrelid = con.conrelid and a.attnum = k.attnum),
                   (select string_agg(a.attname, chr(31) order by k.n) from unnest(con.confkey) with ordinality k(attnum, n)
                      join pg_attribute a on a.attrelid = con.confrelid and a.attnum = k.attnum)
            from pg_constraint con
            join pg_class sc on sc.oid = con.conrelid join pg_namespace sn on sn.oid = sc.relnamespace
            join pg_class tc on tc.oid = con.confrelid join pg_namespace tn on tn.oid = tc.relnamespace
            where con.contype = 'f' and sn.nspname not in ('pg_catalog','information_schema')
            order by 1
            """).rows
        return SchemaGraph.build(columns: columns, links: links)
    }
}

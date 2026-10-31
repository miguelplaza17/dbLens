import Foundation

/// What every database driver gives the app: tables, pages of rows, SQL runs,
/// and row edits. Each driver is an actor that does its blocking client-library
/// calls on its own serial queue.
public protocol DatabaseSession: AnyObject, Sendable {
    nonisolated var engine: Engine { get }
    /// The server's version, for the log ("16.4", "8.4.3", "3.51.0").
    nonisolated var serverVersion: String { get }
    /// The SSH tunnel the connection goes through, if any. It closes with the session.
    nonisolated var tunnel: SSHTunnel? { get }
    /// Stops the running query. Safe to call from any thread.
    nonisolated func cancel()
    /// Quotes a table or column name for this engine's SQL.
    nonisolated func quote(_ identifier: String) -> String

    /// Runs one or more statements, as typed in the SQL editor.
    func execute(_ sql: String) async throws -> Execution
    /// Runs one statement that can only read, for agents. The database itself
    /// refuses writes where it can (a read-only transaction, SQLite's read-only
    /// check); MongoDB and Redis allow only reading methods and commands.
    func readOnly(_ text: String) async throws -> Execution
    func relations() async throws -> [Relation]
    /// One page of rows, sorted by the primary key when there is one, with
    /// column types and, when rows can be edited, their keys. A non-empty
    /// `search` keeps only rows with that text in any column (case-insensitive).
    /// `sort` orders by one column first (then by the key, so pages stay stable).
    func page(of relation: Relation, limit: Int, offset: Int, search: String, sort: ColumnSort?) async throws -> QueryResult
    /// Exact row count (of the rows matching `search`), or nil when counting would be slow.
    func count(of relation: Relation, search: String) async throws -> Int?
    /// Edits exactly one row (rolled back if it would touch more or none).
    func update(_ relation: Relation, keys: RowKeys, row: Int, changes: [(String, String?)]) async throws -> EditOutcome
    func insert(_ relation: Relation, values: [(String, String?)]) async throws -> EditOutcome
    func delete(_ relation: Relation, keys: RowKeys, row: Int) async throws -> EditOutcome
    /// Tables, their columns, and the foreign keys between them, for the schema board.
    func schemaGraph() async throws -> SchemaGraph
}

/// One column to order a table by, chosen by clicking its header.
public struct ColumnSort: Sendable, Hashable {
    public var column: String
    public var ascending: Bool

    public init(column: String, ascending: Bool) {
        self.column = column
        self.ascending = ascending
    }

    /// "asc" or "desc", for SQL.
    public var direction: String { ascending ? "asc" : "desc" }

    /// Text values compared the way people read them: numbers by value, NULL last.
    public static func precedes(_ a: String?, _ b: String?, ascending: Bool) -> Bool {
        switch (a, b) {
        case (nil, nil): return false
        case (nil, _): return false
        case (_, nil): return true
        case let (a?, b?):
            if let x = Double(a), let y = Double(b), x != y { return ascending ? x < y : x > y }
            let order = a.localizedStandardCompare(b)
            return ascending ? order == .orderedAscending : order == .orderedDescending
        }
    }
}

/// `%text%` for LIKE, with LIKE's own wildcards and the escape character escaped by a backslash.
public func likePattern(_ text: String) -> String {
    "%" + text.replacingOccurrences(of: "\\", with: "\\\\")
        .replacingOccurrences(of: "%", with: "\\%")
        .replacingOccurrences(of: "_", with: "\\_") + "%"
}

extension DatabaseSession {
    /// Engines without tables and foreign keys (Redis) have no board.
    public func schemaGraph() async throws -> SchemaGraph { SchemaGraph(tables: [], links: []) }

    /// "schema"."name" in this engine's quoting; just the name when there is no schema.
    public func qualified(_ relation: Relation) -> String {
        relation.schema.isEmpty ? quote(relation.name) : "\(quote(relation.schema)).\(quote(relation.name))"
    }
}

/// One column of a result.
public struct Column: Sendable, Hashable {
    public let name: String
    /// The type name when known ("text", "int(11)", "INTEGER"), or "".
    public var type: String = ""

    public init(name: String, type: String = "") {
        self.name = name
        self.type = type
    }
}

/// One statement's result: rows as text (NULL is nil), or a command tag.
public struct QueryResult: Sendable {
    public var columns: [Column]
    public var rows: [[String?]]
    /// What the statement did: "SELECT 3", "UPDATE 2", "CREATE TABLE".
    public var command: String
    /// True when the result had more rows than `rowLimit` and was cut.
    public var truncated = false
    public var totalRows: Int
    /// How to find each row again for UPDATE and DELETE. Only table pages have it.
    public var keys: RowKeys?
    /// A short line for the footer beside the row count ("hash · TTL 3,600 s").
    public var note: String?

    public var hasRows: Bool { !columns.isEmpty }

    /// Results bigger than this are cut, so the grid stays fast.
    public static let rowLimit = 5_000

    public init(columns: [Column], rows: [[String?]], command: String, truncated: Bool = false,
                totalRows: Int, keys: RowKeys? = nil) {
        self.columns = columns
        self.rows = rows
        self.command = command
        self.truncated = truncated
        self.totalRows = totalRows
        self.keys = keys
    }
}

/// What identifies each row of a table page.
public struct RowKeys: Sendable {
    public enum Kind: Sendable {
        /// The table's primary-key columns.
        case primaryKey
        /// Postgres without a primary key: ctid, checked with xmin.
        case ctid
        /// SQLite without a primary key: the rowid.
        case rowid
    }

    /// The key columns: the primary key, ["ctid", "xmin"], or ["rowid"].
    public let columns: [String]
    public let kind: Kind
    /// One entry per row, in `columns` order.
    public var values: [[String?]]

    public var usesCtid: Bool { kind == .ctid }

    public init(columns: [String], kind: Kind, values: [[String?]]) {
        self.columns = columns
        self.kind = kind
        self.values = values
    }
}

/// What an edit sent to the server, for the log.
public struct EditOutcome: Sendable {
    public let sql: String
    public let command: String
    public let affected: Int

    public init(sql: String, command: String, affected: Int) {
        self.sql = sql
        self.command = command
        self.affected = affected
    }
}

/// Everything one run of the SQL editor returned.
public struct Execution: Sendable {
    public var results: [QueryResult]
    public var duration: Duration
    /// The result to show: the last one with columns, else the last one.
    public var main: QueryResult? { results.last(where: \.hasRows) ?? results.last }

    public init(results: [QueryResult], duration: Duration) {
        self.results = results
        self.duration = duration
    }
}

/// A table or view.
public struct Relation: Sendable, Hashable, Identifiable {
    public enum Kind: String, Sendable { case table, partitioned, view, materialized, foreign, key }
    /// The schema (Postgres), the database (MySQL), "main" (SQLite), or the key's
    /// prefix group (Redis: "user" for "user:42", "" for keys without one).
    public let schema: String
    public let name: String
    public let kind: Kind
    /// The server's row estimate, -1 when unknown.
    public let estimate: Double
    /// What a key holds, for Redis: "string", "hash", "list", "set", "zset", "stream", "ReJSON-RL".
    public let subtype: String

    public init(schema: String, name: String, kind: Kind, estimate: Double, subtype: String = "") {
        self.schema = schema
        self.name = name
        self.kind = kind
        self.estimate = estimate
        self.subtype = subtype
    }

    public var id: String { "\(schema).\(name)" }
    /// "schema"."name" with Postgres' double quotes. Drivers use their own quoting.
    public var qualified: String { "\(quoteIdent(schema)).\(quoteIdent(name))" }
}

/// An error a driver reports, with the server's own message when it has one.
public struct DatabaseError: Error, LocalizedError, Sendable {
    public let message: String
    public init(message: String) { self.message = message }
    public var errorDescription: String? { message }
}

/// The first name, kept so older code and tests read the same.
public typealias PostgresError = DatabaseError

/// The tables of a database and how their foreign keys connect them.
public struct SchemaGraph: Sendable {
    public struct Table: Sendable, Identifiable, Hashable {
        public let schema: String
        public let name: String
        public let isView: Bool
        public var columns: [Column]
        public var id: String { "\(schema).\(name)" }

        public init(schema: String, name: String, isView: Bool, columns: [Column]) {
            self.schema = schema
            self.name = name
            self.isView = isView
            self.columns = columns
        }
    }

    public struct Column: Sendable, Hashable {
        public let name: String
        public let type: String
        public let isPrimaryKey: Bool
        public let isNullable: Bool

        public init(name: String, type: String, isPrimaryKey: Bool, isNullable: Bool) {
            self.name = name
            self.type = type
            self.isPrimaryKey = isPrimaryKey
            self.isNullable = isNullable
        }
    }

    /// A foreign key: `fromColumns` of `from` point at `toColumns` of `to`.
    public struct Link: Sendable, Identifiable, Hashable {
        public let name: String
        public let from: String
        public let fromColumns: [String]
        public let to: String
        public let toColumns: [String]
        public var id: String { "\(from).\(name)" }

        public init(name: String, from: String, fromColumns: [String], to: String, toColumns: [String]) {
            self.name = name
            self.from = from
            self.fromColumns = fromColumns
            self.to = to
            self.toColumns = toColumns
        }
    }

    public var tables: [Table]
    public var links: [Link]

    public init(tables: [Table], links: [Link]) {
        self.tables = tables
        self.links = links
    }

    /// The schemas that have tables, in order of first appearance.
    public var schemas: [String] {
        var seen: [String] = []
        for table in tables where !seen.contains(table.schema) { seen.append(table.schema) }
        return seen
    }
}

extension SchemaGraph {
    /// Builds a graph from two result sets: columns as (schema, table, is view, column,
    /// type, nullable, primary key) with booleans as t/f, 1/0 or true/false; links as
    /// (name, "schema.table", "schema.table", columns, columns) with columns joined by
    /// the unit separator (U+001F).
    static func build(columns: [[String?]], links: [[String?]]) -> SchemaGraph {
        func flag(_ value: String?) -> Bool { ["t", "true", "1", "yes"].contains(value?.lowercased() ?? "") }
        var tables: [Table] = []
        var index: [String: Int] = [:]
        for row in columns where row.count >= 7 {
            let schema = row[0] ?? "", name = row[1] ?? ""
            let id = "\(schema).\(name)"
            if index[id] == nil {
                index[id] = tables.count
                tables.append(Table(schema: schema, name: name, isView: flag(row[2]), columns: []))
            }
            tables[index[id]!].columns.append(Column(name: row[3] ?? "", type: row[4] ?? "",
                                                     isPrimaryKey: flag(row[6]), isNullable: flag(row[5])))
        }
        let separator = Character(UnicodeScalar(31))
        let graphLinks = links.compactMap { row -> Link? in
            guard row.count >= 5, let from = row[1], let to = row[2], index[from] != nil, index[to] != nil else { return nil }
            return Link(name: row[0] ?? "", from: from, fromColumns: (row[3] ?? "").split(separator: separator).map(String.init),
                        to: to, toColumns: (row[4] ?? "").split(separator: separator).map(String.init))
        }
        return SchemaGraph(tables: tables, links: graphLinks)
    }
}

import Foundation
import SwiftData
import DBLensKit

/// dbLens as an MCP server, so agents (Claude Code, Claude Desktop, Cursor…)
/// can read your databases through the connections saved here:
///
///     claude mcp add dblens -- ~/Applications/dbLens.app/Contents/MacOS/dbLens --mcp
///
/// JSON-RPC 2.0 over stdio, one message per line. Agents pick a connection by
/// name and never see its URL or password; SSH tunnels work as in the app.
/// Reading is always read-only (the database refuses writes, see
/// `DatabaseSession.readOnly`); writing needs "Let Agents Write" on the
/// connection. What agents run goes to the connection's SQL history.
enum MCPServer {
    static var requested: Bool { CommandLine.arguments.contains("--mcp") }

    /// Serves until stdin closes, then quits. Never returns.
    static func run() -> Never {
        let (lines, input) = AsyncStream.makeStream(of: String.self)
        Thread.detachNewThread {
            while let line = readLine(strippingNewline: true) { input.yield(line) }
            input.finish()
        }
        Task { @MainActor in
            let server = Server()
            // One message at a time, in order.
            for await line in lines where !line.trimmingCharacters(in: .whitespaces).isEmpty {
                await server.handle(line)
            }
            SSHTunnel.closeAll()
            exit(0)
        }
        dispatchMain()
    }

    static let version = "2025-06-18"
    static let maxRows = 1000
    static let maxValue = 2000

    @MainActor
    final class Server {
        private let container = Store.makeContainer()
        private var sessions: [UUID: any DatabaseSession] = [:]

        init() {
            // Tests (tools/mcp-test.sh): with APP_STORE in /tmp, APP_SEED_URL becomes
            // the connection "test", writable when APP_AGENTS_WRITE=1. URLs stay in memory.
            let env = ProcessInfo.processInfo.environment
            if Store.isOverridden, let url = env["APP_SEED_URL"], !url.isEmpty {
                let connection = SavedConnection(name: "test")
                connection.engine = Engine.detect(url)?.rawValue ?? "postgres"
                connection.agentsCanWrite = env["APP_AGENTS_WRITE"] == "1"
                try? Secrets.setURL(url, for: connection.uuid)
                container.mainContext.insert(connection)
            }
        }

        func handle(_ line: String) async {
            guard let message = (try? JSONSerialization.jsonObject(with: Data(line.utf8))) as? [String: Any] else {
                return send(["jsonrpc": "2.0", "id": NSNull(), "error": ["code": -32700, "message": "Parse error"]])
            }
            // Notifications (no id) need no answer.
            guard let id = message["id"] else { return }
            let method = message["method"] as? String ?? ""
            let params = message["params"] as? [String: Any] ?? [:]
            switch method {
            case "initialize":
                reply(id, [
                    "protocolVersion": params["protocolVersion"] as? String ?? MCPServer.version,
                    "capabilities": ["tools": [:]],
                    "serverInfo": ["name": "dblens",
                                   "version": Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "dev"],
                    "instructions": """
                        Databases saved in dbLens. Start with list_connections, then describe_schema (or \
                        list_tables and describe_table on big databases) before writing queries. query is always read-only. execute changes \
                        data only on connections where the user turned on "Let Agents Write"; try it with \
                        dry_run first where supported, and tell the user what it will change.
                        """,
                ])
            case "ping":
                reply(id, [:])
            case "tools/list":
                reply(id, ["tools": Self.tools])
            case "tools/call":
                let name = params["name"] as? String ?? ""
                let arguments = params["arguments"] as? [String: Any] ?? [:]
                do {
                    let output = try await call(name, arguments)
                    let text = output as? String ?? Self.json(output)
                    reply(id, ["content": [["type": "text", "text": text]], "isError": false])
                } catch {
                    let text = (error as? DatabaseError)?.message ?? error.localizedDescription
                    reply(id, ["content": [["type": "text", "text": text]], "isError": true])
                }
            default:
                send(["jsonrpc": "2.0", "id": id, "error": ["code": -32601, "message": "Method not found: \(method)"]])
            }
        }

        // MARK: Tools

        private func call(_ name: String, _ arguments: [String: Any]) async throws -> Any {
            switch name {
            case "list_connections":
                return try connections().map { connection in
                    ["name": connection.name, "engine": Engine(rawValue: connection.engine)?.name ?? connection.engine,
                     "via_ssh": connection.ssh != nil, "agents_can_write": connection.agentsCanWrite] as [String: Any]
                }
            case "list_tables":
                let (_, session) = try await open(arguments)
                return try await session.relations().map { relation in
                    var item: [String: Any] = ["name": relation.name, "kind": relation.kind.rawValue]
                    if !relation.schema.isEmpty { item["schema"] = relation.schema }
                    if relation.estimate >= 0 { item["estimated_rows"] = Int(relation.estimate) }
                    if !relation.subtype.isEmpty { item["type"] = relation.subtype }
                    return item
                }
            case "describe_schema":
                let (_, session) = try await open(arguments)
                return try await schemaText(arguments["schema"] as? String, in: session)
            case "describe_table":
                let (_, session) = try await open(arguments)
                return try await describe(try text(arguments, "table"), in: session)
            case "query":
                let (connection, session) = try await open(arguments)
                let statement = try text(arguments, "query")
                let limit = min(max(arguments["max_rows"] as? Int ?? 100, 1), MCPServer.maxRows)
                do {
                    let execution = try await session.readOnly(statement)
                    AppModel.recordAgentRun(statement, failed: false, for: connection.uuid)
                    return Self.output(execution, maxRows: limit)
                } catch {
                    AppModel.recordAgentRun(statement, failed: true, for: connection.uuid)
                    throw error
                }
            case "execute":
                let (connection, session) = try await open(arguments)
                let statement = try text(arguments, "statement")
                if arguments["dry_run"] as? Bool == true {
                    var output = Self.output(try await session.rehearse(statement), maxRows: 20)
                    output["dry_run"] = "Run and rolled back: nothing changed."
                    return output
                }
                guard connection.agentsCanWrite else {
                    throw DatabaseError(message: """
                        Writing is off for "\(connection.name)". The user can turn it on in dbLens: right-click \
                        the connection and choose Let Agents Write. Reading with query works.
                        """)
                }
                do {
                    let execution = try await session.execute(statement)
                    AppModel.recordAgentRun(statement, failed: false, for: connection.uuid)
                    return Self.output(execution, maxRows: 100)
                } catch {
                    AppModel.recordAgentRun(statement, failed: true, for: connection.uuid)
                    throw error
                }
            default:
                throw DatabaseError(message: "Unknown tool: \(name)")
            }
        }

        /// Columns, primary key, and foreign keys both ways; for Redis, the key's type and fields.
        private func describe(_ name: String, in session: any DatabaseSession) async throws -> Any {
            let relations = try await session.relations()
            guard let relation = Self.find(name, in: relations) else {
                throw DatabaseError(message: "No table \"\(name)\". list_tables shows what there is.")
            }
            let graph = try await session.schemaGraph()
            if let table = graph.tables.first(where: { $0.schema == relation.schema && $0.name == relation.name }) {
                let outgoing = graph.links.filter { $0.from == table.id }.map {
                    ["columns": $0.fromColumns, "references": $0.to, "referenced_columns": $0.toColumns] as [String: Any]
                }
                let incoming = graph.links.filter { $0.to == table.id && $0.from != table.id }.map {
                    ["table": $0.from, "columns": $0.fromColumns, "referenced_columns": $0.toColumns] as [String: Any]
                }
                return [
                    "table": table.id, "kind": relation.kind.rawValue,
                    "columns": table.columns.map { ["name": $0.name, "type": $0.type, "nullable": $0.isNullable] as [String: Any] },
                    "primary_key": table.columns.filter(\.isPrimaryKey).map(\.name),
                    "foreign_keys": outgoing, "referenced_by": incoming,
                ] as [String: Any]
            }
            // No schema (Redis keys, Mongo collections without a sample): the first rows' shape.
            let sample = try await session.page(of: relation, limit: 5, offset: 0, search: "", sort: nil)
            return [
                "table": relation.id, "kind": relation.kind.rawValue, "type": relation.subtype,
                "columns": sample.columns.map { ["name": $0.name, "type": $0.type] },
                "sample": Self.rows(sample, maxRows: 5),
            ] as [String: Any]
        }

        /// Every table at once, compact enough to read in one go: columns with
        /// type, PK and NOT NULL, then foreign keys.
        private func schemaText(_ schema: String?, in session: any DatabaseSession) async throws -> String {
            let graph = try await session.schemaGraph()
            guard !graph.tables.isEmpty else {
                throw DatabaseError(message: "\(session.engine.name) has no tables and foreign keys to describe; use list_tables.")
            }
            let estimates = Dictionary((try? await session.relations())?.map { ($0.id, $0.estimate) } ?? [], uniquingKeysWith: { a, _ in a })
            let tables = graph.tables.filter { schema == nil || schema!.isEmpty || $0.schema == schema }
            guard !tables.isEmpty else { throw DatabaseError(message: "No tables in schema \"\(schema ?? "")\". Schemas: \(graph.schemas.joined(separator: ", ")).") }
            var lines: [String] = []
            for table in tables {
                var head = "\(table.id) (\(table.isView ? "view" : "table")"
                if let rows = estimates[table.id], rows >= 0 { head += ", ~\(Int(rows)) rows" }
                lines.append(head + ")")
                for column in table.columns {
                    lines.append("  \(column.name) \(column.type)\(column.isPrimaryKey ? " PK" : "")\(column.isNullable || column.isPrimaryKey ? "" : " NOT NULL")")
                }
                for link in graph.links where link.from == table.id {
                    lines.append("  FK (\(link.fromColumns.joined(separator: ", "))) -> \(link.to)(\(link.toColumns.joined(separator: ", ")))")
                }
                lines.append("")
            }
            return lines.joined(separator: "\n")
        }

        // MARK: Connections

        private func connections() throws -> [SavedConnection] {
            try container.mainContext.fetch(FetchDescriptor<SavedConnection>(sortBy: [SortDescriptor(\.name)]))
        }

        /// The named connection, opened once and kept for the next calls.
        private func open(_ arguments: [String: Any]) async throws -> (SavedConnection, any DatabaseSession) {
            let name = try text(arguments, "connection")
            let all = try connections()
            let matches = all.filter { $0.name.caseInsensitiveCompare(name) == .orderedSame || $0.uuid.uuidString == name }
            guard matches.count == 1, let connection = matches.first else {
                let names = all.map(\.name).joined(separator: ", ")
                throw DatabaseError(message: matches.isEmpty ? "No connection \"\(name)\". Saved: \(names)."
                                                             : "Several connections are named \"\(name)\"; rename one in dbLens.")
            }
            if let session = sessions[connection.uuid] { return (connection, session) }
            guard let url = connection.url else {
                throw DatabaseError(message: "dbLens can't read the URL of \"\(connection.name)\". Open dbLens and edit the connection.")
            }
            let session = try await Connector.connect(url, ssh: connection.ssh)
            sessions[connection.uuid] = session
            return (connection, session)
        }

        private func text(_ arguments: [String: Any], _ key: String) throws -> String {
            guard let value = arguments[key] as? String, !value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
                throw DatabaseError(message: "Missing \"\(key)\".")
            }
            return value
        }

        private static func find(_ name: String, in relations: [Relation]) -> Relation? {
            relations.first { $0.id == name || ($0.schema.isEmpty && $0.name == name) }
                ?? { let named = relations.filter { $0.name == name }; return named.count == 1 ? named[0] : nil }()
        }

        // MARK: Output

        /// The main result (the last with rows), and every statement's tag.
        private static func output(_ execution: Execution, maxRows: Int) -> [String: Any] {
            var output: [String: Any] = ["statements": execution.results.map(\.command),
                                         "duration_ms": Int(execution.duration.components.attoseconds / 1_000_000_000_000_000
                                                            + execution.duration.components.seconds * 1000)]
            if let main = execution.main, !main.columns.isEmpty {
                output["columns"] = main.columns.map { $0.type.isEmpty ? $0.name : "\($0.name) (\($0.type))" }
                output["rows"] = rows(main, maxRows: maxRows)
                output["row_count"] = main.totalRows
                if main.totalRows > maxRows || main.truncated {
                    output["truncated"] = "Showing \(min(maxRows, main.rows.count)) of \(main.totalRows) rows; add a LIMIT or raise max_rows (up to \(MCPServer.maxRows))."
                }
            }
            return output
        }

        private static func rows(_ result: QueryResult, maxRows: Int) -> [[Any]] {
            result.rows.prefix(maxRows).map { row in
                row.map { value -> Any in
                    guard let value else { return NSNull() }
                    return value.count > MCPServer.maxValue ? String(value.prefix(MCPServer.maxValue)) + "…" : value
                }
            }
        }

        private static func json(_ value: Any) -> String {
            guard let data = try? JSONSerialization.data(withJSONObject: value, options: [.withoutEscapingSlashes, .fragmentsAllowed])
            else { return "\(value)" }
            return String(decoding: data, as: UTF8.self)
        }

        private func reply(_ id: Any, _ result: Any) {
            send(["jsonrpc": "2.0", "id": id, "result": result])
        }

        private func send(_ message: [String: Any]) {
            guard let data = try? JSONSerialization.data(withJSONObject: message, options: [.withoutEscapingSlashes]) else { return }
            FileHandle.standardOutput.write(data + Data("\n".utf8))
        }

        // MARK: Tool list

        private static let connection: [String: Any] = ["type": "string", "description": "Connection name, from list_connections."]

        static let tools: [[String: Any]] = [
            [
                "name": "list_connections",
                "description": "The database connections saved in dbLens: name, engine (PostgreSQL, MySQL, SQLite, Redis, MongoDB), whether it goes through SSH, and whether agents may write. URLs and passwords are never shown.",
                "inputSchema": ["type": "object", "properties": [:]],
                "annotations": ["readOnlyHint": true],
            ],
            [
                "name": "list_tables",
                "description": "Tables and views of a connection (Redis: keys; MongoDB: collections), with their schema and estimated row count.",
                "inputSchema": ["type": "object", "properties": ["connection": connection], "required": ["connection"]],
                "annotations": ["readOnlyHint": true],
            ],
            [
                "name": "describe_schema",
                "description": "The whole relational structure in one call: every table and view with its columns (type, PK, NOT NULL), estimated rows, and foreign keys. Start here to understand a database. PostgreSQL, MySQL, SQLite, and MongoDB (fields sampled, references guessed).",
                "inputSchema": ["type": "object", "properties": [
                    "connection": connection,
                    "schema": ["type": "string", "description": "Only this schema (PostgreSQL). Default: all."],
                ], "required": ["connection"]],
                "annotations": ["readOnlyHint": true],
            ],
            [
                "name": "describe_table",
                "description": "A table's columns with types and nullability, its primary key, its foreign keys, and the tables that reference it. Use before writing queries.",
                "inputSchema": ["type": "object", "properties": [
                    "connection": connection,
                    "table": ["type": "string", "description": "Table name, or schema.name when several schemas have it."],
                ], "required": ["connection", "table"]],
                "annotations": ["readOnlyHint": true],
            ],
            [
                "name": "query",
                "description": "Runs one read-only statement and returns its rows. SQL for PostgreSQL, MySQL and SQLite (inside a read-only transaction: writes fail); mongosh syntax for MongoDB (db.users.find({...}), aggregate without $out/$merge, countDocuments, distinct); one reading command for Redis (GET, HGETALL, LRANGE, SCAN…).",
                "inputSchema": ["type": "object", "properties": [
                    "connection": connection,
                    "query": ["type": "string", "description": "One statement."],
                    "max_rows": ["type": "integer", "description": "Rows to return, 1 to 1000. Default 100."],
                ], "required": ["connection", "query"]],
                "annotations": ["readOnlyHint": true],
            ],
            [
                "name": "execute",
                "description": "Changes data or schema: INSERT, UPDATE, DELETE, DDL, MongoDB writes, Redis commands. Only on connections where the user turned on Let Agents Write in dbLens. With dry_run (PostgreSQL and SQLite) the statement runs in a transaction that is rolled back, to show what it would affect first. Confirm with the user before running for real.",
                "inputSchema": ["type": "object", "properties": [
                    "connection": connection,
                    "statement": ["type": "string", "description": "What to run."],
                    "dry_run": ["type": "boolean", "description": "Run, report rows affected, and roll back. PostgreSQL and SQLite, one statement."],
                ], "required": ["connection", "statement"]],
                "annotations": ["readOnlyHint": false, "destructiveHint": true],
            ],
        ]
    }
}

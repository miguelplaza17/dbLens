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
        // Agents get English, whatever the Mac's language: set before any text is looked up.
        UserDefaults.standard.setVolatileDomain(["AppleLanguages": ["en"]], forName: UserDefaults.argumentDomain)
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
                try? container.mainContext.save()
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
                        data only on connections where the user turned on "Let Agents Write"; try changes and \
                        migrations with test_changes first, and tell the user what they will change.
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
            case "create_connection":
                return try await createConnection(arguments)
            case "delete_connection":
                return try deleteConnection(arguments)
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
            case "list_extensions":
                let (_, session) = try await open(arguments)
                return try await extensions(in: session)
            case "install_extension":
                let (connection, session) = try await open(arguments)
                return try await install(try text(arguments, "name"), schema: arguments["schema"] as? String,
                                         on: connection, session)
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
            case "test_changes":
                let (connection, session) = try await open(arguments)
                let script = try text(arguments, "statements")
                try requireWrite(connection, "Testing runs the statements (and locks what they touch) before undoing them")
                do {
                    var output = Self.output(try await session.rehearse(script), maxRows: 20)
                    output["rolled_back"] = "Every statement ran and was undone: nothing changed."
                    AppModel.recordAgentRun("-- test\n" + script, failed: false, for: connection.uuid)
                    return output
                } catch {
                    AppModel.recordAgentRun("-- test\n" + script, failed: true, for: connection.uuid)
                    throw DatabaseError(message: ((error as? DatabaseError)?.message ?? error.localizedDescription)
                                        + " (Rolled back: nothing changed.)")
                }
            case "execute":
                let (connection, session) = try await open(arguments)
                let script = try text(arguments, "statements")
                try requireWrite(connection, "Writing")
                let atomic = arguments["atomic"] as? Bool
                do {
                    var output: [String: Any]
                    if atomic == false {
                        output = Self.output(try await session.execute(script), maxRows: 100)
                        output["atomic"] = false
                    } else {
                        do {
                            output = Self.output(try await session.executeAtomically(script), maxRows: 100)
                            output["atomic"] = "All statements committed together."
                        } catch let refused as NotAtomic {
                            // Asked for explicitly: say why not. Otherwise run plainly and say what that means.
                            if atomic == true { throw refused }
                            output = Self.output(try await session.execute(script), maxRows: 100)
                            output["atomic"] = false
                            output["note"] = refused.message + " " + Self.nonAtomicNote(session.engine)
                        }
                    }
                    AppModel.recordAgentRun(script, failed: false, for: connection.uuid)
                    return output
                } catch let refused as NotAtomic {
                    throw DatabaseError(message: refused.message + " Nothing ran; pass atomic: false to run it anyway.")
                } catch {
                    AppModel.recordAgentRun(script, failed: true, for: connection.uuid)
                    let message = (error as? DatabaseError)?.message ?? error.localizedDescription
                    if atomic != false, [.postgres, .sqlite].contains(session.engine) {
                        throw DatabaseError(message: message + " (Rolled back: nothing changed.)")
                    }
                    throw DatabaseError(message: message + " " + Self.nonAtomicNote(session.engine))
                }
            default:
                throw DatabaseError(message: "Unknown tool: \(name)")
            }
        }

        private func requireWrite(_ connection: SavedConnection, _ what: String) throws {
            guard connection.agentsCanWrite else {
                throw DatabaseError(message: """
                    \(what) is off for "\(connection.name)". The user can turn it on in dbLens: right-click the \
                    connection (or Settings > Agents) and choose Let Agents Write. Reading with query works.
                    """)
            }
        }

        private static func nonAtomicNote(_ engine: Engine) -> String {
            switch engine {
            case .mysql: "MySQL commits DDL as it runs: statements before a failing one stay applied. Check with describe_schema."
            case .mongodb: "MongoDB ran each statement on its own: the ones before a failure stay applied."
            case .redis: "Redis ran each command on its own: the ones before a failure stay applied."
            default: "Statements before a failure stay applied."
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

        /// What the database adds to plain SQL, so queries only use functions that
        /// exist: Postgres extensions (installed and available), MySQL plugins,
        /// SQLite's compile options. Always with the server version.
        private func extensions(in session: any DatabaseSession) async throws -> Any {
            func rows(_ sql: String) async throws -> [[String?]] { try await session.readOnly(sql).main?.rows ?? [] }
            var output: [String: Any] = ["engine": session.engine.name, "version": session.serverVersion]
            switch session.engine {
            case .postgres:
                let all = try await rows("""
                    select a.name, a.installed_version, a.default_version, n.nspname, a.comment
                    from pg_available_extensions a
                    left join pg_extension e on e.extname = a.name
                    left join pg_namespace n on n.oid = e.extnamespace
                    order by a.name
                    """)
                output["installed"] = all.filter { $0[1] != nil }.map {
                    ["name": $0[0] ?? "", "version": $0[1] ?? "", "schema": $0[3] ?? "", "comment": $0[4] ?? ""]
                }
                output["available"] = all.filter { $0[1] == nil }.map {
                    ["name": $0[0] ?? "", "version": $0[2] ?? "", "comment": $0[4] ?? ""]
                }
                output["note"] = "install_extension adds an available one (needs Let Agents Write and the right privileges)."
            case .mysql:
                output["plugins"] = try await rows("""
                    select PLUGIN_NAME, PLUGIN_TYPE from information_schema.PLUGINS
                    where PLUGIN_STATUS = 'ACTIVE' order by PLUGIN_TYPE, PLUGIN_NAME
                    """).map { ["name": $0[0] ?? "", "type": $0[1] ?? ""] }
            case .sqlite:
                output["compile_options"] = try await rows("select compile_options from pragma_compile_options").compactMap { $0.first ?? nil }
            case .redis:
                output["modules"] = try await session.execute("MODULE LIST").main?.rows ?? []
            case .mongodb:
                output["note"] = "MongoDB has no extensions; features depend on the server version."
            }
            return output
        }

        /// CREATE EXTENSION, for Postgres, where agents may write. The name must be
        /// one pg_available_extensions lists.
        private func install(_ name: String, schema: String?, on connection: SavedConnection,
                             _ session: any DatabaseSession) async throws -> Any {
            guard session.engine == .postgres else {
                throw DatabaseError(message: "Only PostgreSQL has extensions to install; \(session.engine.name) doesn't.")
            }
            guard connection.agentsCanWrite else {
                throw DatabaseError(message: """
                    Writing is off for "\(connection.name)", and installing an extension is a change. The user can \
                    turn it on in dbLens: right-click the connection and choose Let Agents Write.
                    """)
            }
            let available = try await session.readOnly("select name, installed_version from pg_available_extensions").main?.rows ?? []
            guard let row = available.first(where: { $0[0] == name }) else {
                throw DatabaseError(message: "\"\(name)\" isn't available on this server; list_extensions shows what is.")
            }
            if let version = row[1] { return ["name": name, "version": version, "note": "Already installed."] }
            var statement = "create extension if not exists \(session.quote(name))"
            if let schema, !schema.isEmpty { statement += " schema \(session.quote(schema))" }
            do {
                _ = try await session.execute(statement)
                AppModel.recordAgentRun(statement, failed: false, for: connection.uuid)
            } catch {
                AppModel.recordAgentRun(statement, failed: true, for: connection.uuid)
                throw error
            }
            let version = try await session.readOnly("select extversion from pg_extension where extname = \(Self.literal(name))")
                .main?.rows.first?.first ?? nil
            return ["name": name, "version": version ?? "", "statement": statement]
        }

        private static func literal(_ text: String) -> String { "'" + text.replacingOccurrences(of: "'", with: "''") + "'" }

        // MARK: Connections

        /// Settings > Agents > "Let agents add and remove connections", off by default.
        private func requireManage() throws {
            let allowed = Store.isOverridden ? ProcessInfo.processInfo.environment["APP_AGENTS_WRITE"] == "1"  // tests
                                             : UserDefaults.standard.bool(forKey: "agentsManageConnections")
            guard allowed else {
                throw DatabaseError(message: """
                    Adding and removing connections is off. The user can turn it on in dbLens: Settings > Agents > \
                    Let agents add and remove connections.
                    """)
            }
        }

        /// Connects first (so a wrong URL fails here, with its cause), then saves
        /// the URL encrypted, like the app's New Connection. Writing stays off.
        private func createConnection(_ arguments: [String: Any]) async throws -> Any {
            try requireManage()
            let url = try text(arguments, "url")
            guard let engine = Engine.detect(url) else {
                throw DatabaseError(message: "This doesn't look like a database URL (postgres://, mysql://, sqlite:///path, redis://, mongodb://).")
            }
            var ssh: SSHTunnel.Config?
            if let host = arguments["ssh_host"] as? String, !host.isEmpty {
                ssh = SSHTunnel.Config(host: host, port: arguments["ssh_port"] as? Int,
                                       user: arguments["ssh_user"] as? String ?? "", keyPath: arguments["ssh_key"] as? String ?? "")
            }
            let session = try await Connector.connect(url, ssh: ssh)
            let given = (arguments["name"] as? String)?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
            let name = given.isEmpty ? (Connector.summary(url)?.suggestedName ?? engine.name) : given
            guard try !connections().contains(where: { $0.name.caseInsensitiveCompare(name) == .orderedSame }) else {
                session.tunnel?.close()
                throw DatabaseError(message: "A connection named \"\(name)\" already exists; pass another name.")
            }
            let connection = SavedConnection(name: name)
            connection.engine = engine.rawValue
            connection.ssh = ssh
            try Secrets.setURL(Connector.clean(url), for: connection.uuid)
            let context = ModelContext(container)
            context.insert(connection)
            try context.save()
            sessions[connection.uuid] = session
            Self.announce()
            return ["name": name, "engine": engine.name, "server_version": session.serverVersion,
                    "note": "Saved in dbLens, URL encrypted. Agents can read it now; writing stays off until the user turns it on."]
        }

        private func deleteConnection(_ arguments: [String: Any]) throws -> Any {
            try requireManage()
            let name = try text(arguments, "connection")
            let matches = try connections().filter { $0.name.caseInsensitiveCompare(name) == .orderedSame || $0.uuid.uuidString == name }
            guard matches.count == 1, let connection = matches.first else {
                throw DatabaseError(message: matches.isEmpty ? "No connection \"\(name)\"." : "Several connections are named \"\(name)\"; rename one in dbLens.")
            }
            sessions.removeValue(forKey: connection.uuid)?.tunnel?.close()
            Secrets.removeURL(for: connection.uuid)
            UserDefaults.standard.removeObject(forKey: "sqlHistory.\(connection.uuid.uuidString)")
            if let context = connection.modelContext {
                context.delete(connection)
                try context.save()
            }
            Self.announce()
            return ["deleted": name, "note": "Removed from dbLens with its saved URL and SQL history. The database itself is untouched."]
        }

        /// Tells a running dbLens to read its connections again.
        private static func announce() {
            DistributedNotificationCenter.default().postNotificationName(AppModel.connectionsChanged, object: nil,
                                                                         userInfo: nil, deliverImmediately: true)
        }

        /// Read from a new context on every call: the app may have changed a
        /// connection (Let Agents Write, a rename) since the last one.
        private func connections() throws -> [SavedConnection] {
            try ModelContext(container).fetch(FetchDescriptor<SavedConnection>(sortBy: [SortDescriptor(\.name)]))
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
                "description": "The database connections saved in dbLens: name, engine (PostgreSQL, MySQL, SQLite, Redis, MongoDB), whether it goes through SSH, and whether agents may write. URLs and passwords are never shown. Databases: all (PostgreSQL, MySQL/MariaDB, SQLite, Redis/Valkey, MongoDB).",
                "inputSchema": ["type": "object", "properties": [:]],
                "annotations": ["readOnlyHint": true],
            ],
            [
                "name": "create_connection",
                "description": "Saves a new connection in dbLens from its URL, after connecting to check it (a failure says why: refused, unknown host, wrong password…). The URL is stored encrypted; afterwards only the name is used. Optionally through SSH. Writing stays off on it until the user allows it. Needs the user's Settings > Agents > Let agents add and remove connections. Databases: all.",
                "inputSchema": ["type": "object", "properties": [
                    "url": ["type": "string", "description": "Full connection URL: postgres://, mysql://, mariadb://, sqlite:///path, redis://, rediss://, mongodb://, mongodb+srv://."],
                    "name": ["type": "string", "description": "Name to show. Default: from the URL (user @ host)."],
                    "ssh_host": ["type": "string", "description": "Connect through this SSH host (a ~/.ssh/config alias works)."],
                    "ssh_user": ["type": "string"],
                    "ssh_port": ["type": "integer"],
                    "ssh_key": ["type": "string", "description": "Path to a private key; default: ssh-agent and ~/.ssh/config."],
                ], "required": ["url"]],
                "annotations": ["readOnlyHint": false, "destructiveHint": false],
            ],
            [
                "name": "delete_connection",
                "description": "Removes a saved connection from dbLens, with its encrypted URL and SQL history. The database itself is not touched. Confirm with the user first. Needs the user's Settings > Agents > Let agents add and remove connections. Databases: all.",
                "inputSchema": ["type": "object", "properties": ["connection": connection], "required": ["connection"]],
                "annotations": ["readOnlyHint": false, "destructiveHint": true],
            ],
            [
                "name": "list_tables",
                "description": "Tables and views of a connection (Redis: keys; MongoDB: collections), with their schema and estimated row count. Databases: all.",
                "inputSchema": ["type": "object", "properties": ["connection": connection], "required": ["connection"]],
                "annotations": ["readOnlyHint": true],
            ],
            [
                "name": "describe_schema",
                "description": "The whole relational structure in one call: every table and view with its columns (type, PK, NOT NULL), estimated rows, and foreign keys. Start here to understand a database. Databases: PostgreSQL, MySQL/MariaDB, SQLite, MongoDB (fields sampled, references guessed). Not Redis: use list_tables.",
                "inputSchema": ["type": "object", "properties": [
                    "connection": connection,
                    "schema": ["type": "string", "description": "Only this schema (PostgreSQL). Default: all."],
                ], "required": ["connection"]],
                "annotations": ["readOnlyHint": true],
            ],
            [
                "name": "list_extensions",
                "description": "What the database adds to plain SQL, to write queries only with functions that exist: PostgreSQL extensions (installed with version and schema, and available to install), MySQL active plugins, SQLite compile options (JSON1, FTS5…), Redis modules. Always with the server version. Databases: all; MongoDB only reports its version.",
                "inputSchema": ["type": "object", "properties": ["connection": connection], "required": ["connection"]],
                "annotations": ["readOnlyHint": true],
            ],
            [
                "name": "install_extension",
                "description": "Installs an available PostgreSQL extension (CREATE EXTENSION). Only where the user turned on Let Agents Write, and the database user needs the privilege. Confirm with the user first. Databases: PostgreSQL only.",
                "inputSchema": ["type": "object", "properties": [
                    "connection": connection,
                    "name": ["type": "string", "description": "Extension name, from list_extensions' available list."],
                    "schema": ["type": "string", "description": "Schema to put it in. Default: the server's choice."],
                ], "required": ["connection", "name"]],
                "annotations": ["readOnlyHint": false, "destructiveHint": false],
            ],
            [
                "name": "describe_table",
                "description": "A table's columns with types and nullability, its primary key, its foreign keys, and the tables that reference it. Use before writing queries. Databases: all. Redis and MongoDB: the fields of the first entries, with types.",
                "inputSchema": ["type": "object", "properties": [
                    "connection": connection,
                    "table": ["type": "string", "description": "Table name, or schema.name when several schemas have it."],
                ], "required": ["connection", "table"]],
                "annotations": ["readOnlyHint": true],
            ],
            [
                "name": "query",
                "description": "Runs one read-only statement and returns its rows. SQL for PostgreSQL and MySQL (inside a read-only transaction: writes fail) and SQLite (only statements SQLite itself says change nothing); mongosh syntax for MongoDB (db.users.find({...}), aggregate without $out/$merge, countDocuments, distinct); one reading command for Redis (GET, HGETALL, LRANGE, SCAN…). Databases: all.",
                "inputSchema": ["type": "object", "properties": [
                    "connection": connection,
                    "query": ["type": "string", "description": "One statement."],
                    "max_rows": ["type": "integer", "description": "Rows to return, 1 to 1000. Default 100."],
                ], "required": ["connection", "query"]],
                "annotations": ["readOnlyHint": true],
            ],
            [
                "name": "test_changes",
                "description": "Tries a change or a whole migration without keeping it: runs every statement inside a transaction that is always rolled back, and reports what each did (rows affected) or which one failed and why. PostgreSQL and SQLite: data and schema (DDL). MySQL: data changes only, since its DDL commits by itself. MongoDB and Redis can't undo, so they refuse. Use before execute. Leave out BEGIN/COMMIT: dbLens owns the transaction. Needs Let Agents Write, since the statements really run (and lock) before the rollback. Databases: PostgreSQL and SQLite (everything), MySQL/MariaDB (data changes only). Not MongoDB or Redis.",
                "inputSchema": ["type": "object", "properties": [
                    "connection": connection,
                    "statements": ["type": "string", "description": "One statement or a script, separated by semicolons."],
                ], "required": ["connection", "statements"]],
                "annotations": ["readOnlyHint": false, "destructiveHint": false, "idempotentHint": true],
            ],
            [
                "name": "execute",
                "description": "Changes data or schema, for real: INSERT, UPDATE, DELETE, migrations (DDL), MongoDB writes, Redis commands. Only on connections where the user turned on Let Agents Write in dbLens. Scripts run all or nothing where the database allows (PostgreSQL, SQLite, MySQL without DDL); otherwise the result says so. Run test_changes first and confirm with the user. If the project has a migration tool (Prisma, Rails, Flyway…), use the tool instead, so it records the migration. Databases: all. All or nothing on PostgreSQL, SQLite, and MySQL/MariaDB without DDL.",
                "inputSchema": ["type": "object", "properties": [
                    "connection": connection,
                    "statements": ["type": "string", "description": "One statement or a script, separated by semicolons. Leave out BEGIN/COMMIT."],
                    "atomic": ["type": "boolean", "description": "true: all or nothing, or refuse when the database can't. false: plain, each statement on its own. Default: all or nothing where possible."],
                ], "required": ["connection", "statements"]],
                "annotations": ["readOnlyHint": false, "destructiveHint": true],
            ],
        ]
    }
}

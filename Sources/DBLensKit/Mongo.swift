import CMongoC
import Foundation
import Synchronization

/// A MongoDB connection string, as pasted.
///   mongodb://ana:pw@db.example.com:27017/shop?authSource=admin
///   mongodb://h1:27017,h2:27017,h3:27017/shop?replicaSet=rs0
///   mongodb+srv://ana:pw@cluster0.example.mongodb.net/shop?retryWrites=true&w=majority
///   MONGODB_URI="mongodb+srv://…"                                  (an .env line)
/// Query options go to libmongoc unchanged, so everything it knows works.
public struct MongoURL: Sendable, Hashable {
    /// What was pasted, without an .env name or quotes around it.
    public let raw: String
    /// mongodb+srv://: the hosts come from DNS.
    public let isSRV: Bool
    /// "host:port" for each host ("host" alone for mongodb+srv://, "[::1]:27017" for IPv6).
    public let hosts: [String]
    public let user: String?
    public let password: String?
    /// The database in the path, or nil when there is none.
    public let database: String?

    /// Each host and its port (27017 when not given).
    let endpoints: [Endpoint]

    struct Endpoint: Sendable, Hashable {
        let host: String
        let port: Int
    }

    /// "shop @ cluster0.example.mongodb.net", or "mongodb @ host" without a database.
    public var suggestedName: String { "\(database ?? "mongodb") @ \(endpoints.first?.host ?? "localhost")" }

    public init(_ text: String) throws {
        let cleaned = ConnectionURL.clean(text)
        guard !cleaned.isEmpty else { throw DatabaseError(message: String(localized: "Paste the database URL.")) }
        guard let parts = URLParts(cleaned), parts.scheme == "mongodb" || parts.scheme == "mongodb+srv",
              cleaned.range(of: "^(?i)mongodb(\\+srv)?://", options: .regularExpression) != nil else {
            throw DatabaseError(message: String(localized: "Unrecognized format. Use a mongodb://… URL (or mongodb+srv://)"))
        }
        raw = cleaned
        isSRV = parts.scheme == "mongodb+srv"
        guard !parts.hosts.isEmpty, parts.hosts.allSatisfy({ !$0.name.isEmpty }) else {
            throw DatabaseError(message: String(localized: "The URL needs a host, like mongodb://localhost:27017."))
        }
        endpoints = parts.hosts.map { Endpoint(host: $0.name, port: $0.port ?? 27017) }
        hosts = parts.hosts.map { host in
            let name = host.name.contains(":") ? "[\(host.name)]" : host.name
            return parts.scheme == "mongodb+srv" ? name : "\(name):\(host.port ?? 27017)"
        }
        user = parts.user.flatMap { $0.isEmpty ? nil : $0 }
        password = parts.password
        database = parts.path.isEmpty ? nil : parts.path
        // libmongoc's own check, so a URL it would refuse is refused here, in its words.
        MongoLibrary.prepare()
        var error = bson_error_t()
        guard let uri = mongoc_uri_new_with_error(connectionText, &error) else {
            throw DatabaseError(message: BSON.message(&error))
        }
        mongoc_uri_destroy(uri)
    }

    /// The text libmongoc gets: the scheme in lowercase, the rest as pasted.
    var connectionText: String {
        guard let range = raw.range(of: "://") else { return raw }
        return raw[..<range.lowerBound].lowercased() + raw[range.lowerBound...]
    }

    /// The same URL pointed at an SSH tunnel's local end, connecting to that one server directly.
    func text(throughLocalPort port: Int) -> String {
        let text = connectionText
        guard let scheme = text.range(of: "://") else { return text }
        let rest = text[scheme.upperBound...]
        let authorityEnd = rest.firstIndex { $0 == "/" || $0 == "?" } ?? rest.endIndex
        let authority = rest[..<authorityEnd]
        let userinfo = authority.lastIndex(of: "@").map { String(authority[...$0]) } ?? ""
        var path = String(rest[authorityEnd...])
        var query: [String] = []
        if let mark = path.firstIndex(of: "?") {
            query = path[path.index(after: mark)...].split(separator: "&").map(String.init)
            path = String(path[..<mark])
        }
        if path.isEmpty { path = "/" }
        query.removeAll { $0.lowercased().hasPrefix("directconnection=") }
        query.append("directConnection=true")
        return "mongodb://\(userinfo)127.0.0.1:\(port)\(path)?\(query.joined(separator: "&"))"
    }
}

/// libmongoc's process-wide setup.
enum MongoLibrary {
    private static let ready: Void = {
        mongoc_init()
        // libmongoc logs warnings to stderr; dbLens shows errors where they happen instead.
        mongoc_log_set_handler({ _, _, _, _ in }, nil)
    }()

    static func prepare() { _ = ready }
}

/// One MongoDB connection (a libmongoc client). libmongoc blocks, so every call
/// runs on the session's own serial queue, never on the main thread or the shared pool.
///
/// Collections are the relations (schema = database). Pages and query results are
/// tables: `_id` first, then every top-level field in order of first appearance;
/// nested documents and arrays show as compact relaxed extended JSON. The command
/// panel takes mongosh-style statements (see `MongoShell`).
public actor MongoSession: DatabaseSession {
    public nonisolated var engine: Engine { .mongodb }
    /// MongoDB names need no quoting.
    public nonisolated func quote(_ identifier: String) -> String { identifier }

    private let queue = DispatchSerialQueue(label: "dbLens.mongoc")
    public nonisolated var unownedExecutor: UnownedSerialExecutor { queue.asUnownedSerialExecutor() }

    /// Only touched on `queue`, and by deinit once nothing else can.
    nonisolated(unsafe) private let client: OpaquePointer
    private let canceller: MongoCanceller
    public nonisolated let url: MongoURL
    public nonisolated let serverVersion: String
    /// The SSH tunnel this connection goes through, if any. It closes with the session.
    public nonisolated let tunnel: SSHTunnel?

    /// Where the command panel's statements run: the URL's database (else "test",
    /// as in mongosh) until a `use` switches it.
    public private(set) var currentDatabase: String
    /// Each collection's column types from its last page, for inserts.
    private var columnTypes: [String: [String: String]] = [:]

    /// Documents a cursor asks for at a time, so a cancel is seen between batches.
    static let batchSize = 500

    /// Connects, through an SSH tunnel to the server's host and port (as the SSH
    /// server sees them) when `ssh` is given, or throws libmongoc's own message.
    /// A tunnel takes one host and no mongodb+srv://.
    public static func connect(_ url: MongoURL, ssh: SSHTunnel.Config?) async throws -> MongoSession {
        guard let ssh else { return try await Task.detached { try MongoSession(url: url, tunnel: nil) }.value }
        guard !url.isSRV, url.endpoints.count == 1, let endpoint = url.endpoints.first else {
            throw DatabaseError(message: String(localized: "An SSH tunnel goes to one MongoDB server. Use a mongodb:// URL with a single host (not mongodb+srv://)."))
        }
        let tunnel = try await SSHTunnel.open(ssh, to: endpoint.host, port: endpoint.port)
        do {
            return try await Task.detached { try MongoSession(url: url, tunnel: tunnel) }.value
        } catch {
            tunnel.close()
            throw error
        }
    }

    private init(url: MongoURL, tunnel: SSHTunnel?) throws {
        MongoLibrary.prepare()
        let text = tunnel.map { url.text(throughLocalPort: $0.localPort) } ?? url.connectionText
        let client = try Self.openClient(text)
        do {
            try Self.command(client, database: "admin", #"{"ping":1}"#) { _ in }
            var version = ""
            try Self.command(client, database: "admin", #"{"buildInfo":1}"#) { reply in
                var iter = bson_iter_t()
                if bson_iter_init_find(&iter, reply, "version"), let text = BSON.cell(&iter) { version = text }
            }
            serverVersion = version
        } catch {
            mongoc_client_destroy(client)
            throw error
        }
        self.client = client
        self.url = url
        self.tunnel = tunnel
        currentDatabase = url.database ?? "test"
        canceller = MongoCanceller(uri: text)
    }

    deinit {
        mongoc_client_destroy(client)
        tunnel?.close()
    }

    /// A client for the URI, with 10-second timeouts unless the URL sets its own.
    static func openClient(_ text: String) throws -> OpaquePointer {
        var error = bson_error_t()
        guard let uri = mongoc_uri_new_with_error(text, &error) else { throw DatabaseError(message: BSON.message(&error)) }
        defer { mongoc_uri_destroy(uri) }
        for option in [MONGOC_URI_SERVERSELECTIONTIMEOUTMS, MONGOC_URI_CONNECTTIMEOUTMS] where !mongoc_uri_has_option(uri, option) {
            mongoc_uri_set_option_as_int32(uri, option, 10_000)
        }
        if !mongoc_uri_has_option(uri, MONGOC_URI_APPNAME) { mongoc_uri_set_appname(uri, "dbLens") }
        guard let client = mongoc_client_new_from_uri_with_error(uri, &error) else {
            throw DatabaseError(message: BSON.message(&error))
        }
        mongoc_client_set_error_api(client, 2)
        return client
    }

    /// Runs a command given as extended JSON and hands its reply to `body`.
    static func command(_ client: OpaquePointer, database: String, _ json: String,
                        _ body: (UnsafePointer<bson_t>) throws -> Void) throws {
        let doc = try BSONDocument(json: json)
        try Self.command(client, database: database, doc.pointer, body)
    }

    static func command(_ client: OpaquePointer, database: String, _ command: UnsafePointer<bson_t>,
                        _ body: (UnsafePointer<bson_t>) throws -> Void) throws {
        try BSON.withReply { reply in
            var error = bson_error_t()
            guard mongoc_client_command_simple(client, database, command, nil, reply, &error) else {
                throw DatabaseError(message: BSON.message(&error))
            }
            try body(reply)
        }
    }

    // MARK: Canceling

    /// Stops the running operation. Safe to call from any thread; it never waits
    /// for the server. A second, short connection finds the operation by the
    /// comment every read carries and kills it (killOp); cursors also stop
    /// between batches. libmongoc itself can't cancel, so a single write or
    /// `runCommand` (which carry no comment) finishes before the cancel is seen.
    public nonisolated func cancel() { canceller.cancel() }

    static var canceled: DatabaseError { DatabaseError(message: String(localized: "The query was canceled.")) }

    /// Runs `body` as one cancelable operation; it gets the comment to tag its reads with.
    private func cancelable<T>(_ body: (String) throws -> T) throws -> T {
        let tag = canceller.begin()
        let outcome = Result { try body(tag) }
        if canceller.end() { throw Self.canceled }
        return try outcome.get()
    }

    // MARK: Relations

    /// The URL's database, or every database but admin, local and config; in each,
    /// the collections (as tables) and views, without system.* collections.
    public func relations() throws -> [Relation] {
        let databases = try url.database.map { [$0] } ?? databaseNames().filter { !["admin", "local", "config"].contains($0) }
        return try databases.sorted().flatMap { try collections(in: $0) }
    }

    private func databaseNames() throws -> [String] {
        let opts = try BSONDocument(json: #"{"authorizedDatabases":true}"#)
        var error = bson_error_t()
        guard let names = mongoc_client_get_database_names_with_opts(client, opts.pointer, &error) else {
            throw DatabaseError(message: BSON.message(&error))
        }
        defer { bson_strfreev(names) }
        var result: [String] = []
        var index = 0
        while let name = names[index] {
            result.append(String(cString: name))
            index += 1
        }
        return result
    }

    private func collections(in database: String) throws -> [Relation] {
        var found: [(name: String, type: String)] = []
        let db = mongoc_client_get_database(client, database)
        defer { mongoc_database_destroy(db) }
        let opts = try BSONDocument(json: #"{"nameOnly":true,"authorizedCollections":true}"#)
        try drain(mongoc_database_find_collections_with_opts(db, opts.pointer), limit: .max) { doc in
            var iter = bson_iter_t()
            let name = bson_iter_init_find(&iter, doc, "name") ? BSON.cell(&iter) ?? "" : ""
            let type = bson_iter_init_find(&iter, doc, "type") ? BSON.cell(&iter) ?? "collection" : "collection"
            if !name.isEmpty, !name.hasPrefix("system.") { found.append((name, type)) }
        }
        return found.sorted { $0.name < $1.name }.map { item in
            let isView = item.type == "view"
            return Relation(schema: database, name: item.name, kind: isView ? .view : .table,
                            estimate: isView ? -1 : estimatedCount(database, item.name), subtype: item.type)
        }
    }

    /// The count from the collection's metadata (fast), or -1.
    private func estimatedCount(_ database: String, _ collection: String) -> Double {
        withCollection(database, collection) { coll in
            var error = bson_error_t()
            let count = mongoc_collection_estimated_document_count(coll, nil, nil, nil, &error)
            return count < 0 ? -1 : Double(count)
        }
    }

    @discardableResult
    private func withCollection<T>(_ database: String, _ name: String, _ body: (OpaquePointer) throws -> T) rethrows -> T {
        let coll = mongoc_client_get_collection(client, database, name)!
        defer { mongoc_collection_destroy(coll) }
        return try body(coll)
    }

    /// Reads a cursor (and destroys it): up to `limit` documents, stopping when a
    /// cancel comes. True when more documents were left.
    @discardableResult
    private func drain(_ cursor: OpaquePointer?, limit: Int, _ body: (UnsafePointer<bson_t>) throws -> Void) throws -> Bool {
        guard let cursor else { throw DatabaseError(message: String(localized: "No result.")) }
        defer { mongoc_cursor_destroy(cursor) }
        var count = 0
        var doc: UnsafePointer<bson_t>?
        while mongoc_cursor_next(cursor, &doc) {
            if canceller.requested { throw Self.canceled }
            guard let doc else { continue }
            if count >= limit { return true }
            try body(doc)
            count += 1
        }
        var error = bson_error_t()
        if mongoc_cursor_error(cursor, &error) { throw DatabaseError(message: BSON.message(&error)) }
        return false
    }

    // MARK: Pages

    /// One page of a collection, sorted by _id, as a table (see the type's notes).
    /// Rows are found again by their _id, in canonical extended JSON.
    public func page(of relation: Relation, limit: Int, offset: Int, search: String = "", sort: ColumnSort? = nil) throws -> QueryResult {
        // The search needs the field names, which come from a page without it.
        if !search.isEmpty, columnTypes[relation.id] == nil { _ = try page(of: relation, limit: 1, offset: 0) }
        return try cancelable { tag in
            let order = sort.map { #"\#(MongoShell.jsonString($0.column)):\#($0.ascending ? 1 : -1),"# } ?? ""
            var options = [#""sort":{\#(order)"_id":1}"#, #""skip":\#(max(0, offset))"#, #""batchSize":\#(Self.batchSize)"#,
                           #""comment":\#(MongoShell.jsonString(tag))"#]
            if limit > 0 { options.append(#""limit":\#(limit)"#) }
            let opts = try BSONDocument(json: "{" + options.joined(separator: ",") + "}")
            let filter = try BSONDocument(json: searchFilter(search, in: relation))
            var table = DocumentTable(keepIDs: relation.kind != .view)
            try withCollection(relation.schema, relation.name) { coll in
                try drain(mongoc_collection_find_with_opts(coll, filter.pointer, opts.pointer, nil), limit: .max) { table.add($0) }
            }
            var result = table.result(command: "FIND \(table.rows.count)")
            if relation.kind != .view {
                result.keys = RowKeys(columns: ["_id"], kind: .primaryKey, values: table.orderedIDs.map { [$0] })
            }
            if relation.estimate >= 0 {
                let count = Int(relation.estimate)
                result.note = count == 1 ? String(localized: "1 document") : String(localized: "\(count.formatted()) documents")
            }
            columnTypes[relation.id] = Dictionary(result.columns.map { ($0.name, $0.type) }, uniquingKeysWith: { a, _ in a })
            return result
        }
    }

    /// countDocuments({}), or nil when the estimate is over 500,000.
    public func count(of relation: Relation, search: String = "") throws -> Int? {
        if relation.estimate > 500_000 { return nil }
        if !search.isEmpty, columnTypes[relation.id] == nil { _ = try page(of: relation, limit: 1, offset: 0) }
        let filter = searchFilter(search, in: relation)
        return try cancelable { tag in
            try countDocuments(relation.schema, relation.name, filter: filter, tag: tag)
        }
    }

    /// Documents with the text in any top-level field (as a case-insensitive
    /// regex on text fields, and on _id as text). The fields are the ones the
    /// last page showed.
    private func searchFilter(_ search: String, in relation: Relation) -> String {
        guard !search.isEmpty else { return "{}" }
        let pattern = MongoShell.jsonString(NSRegularExpression.escapedPattern(for: search))
        var clauses = (columnTypes[relation.id]?.keys.sorted() ?? []).filter { $0 != "_id" }.map {
            #"{\#(MongoShell.jsonString($0)):{"$regex":\#(pattern),"$options":"i"}}"#
        }
        clauses.append(#"{"$expr":{"$regexMatch":{"input":{"$toString":"$_id"},"regex":\#(pattern),"options":"i"}}}"#)
        return #"{"$or":[\#(clauses.joined(separator: ","))]}"#
    }

    private func countDocuments(_ database: String, _ collection: String, filter: String, tag: String) throws -> Int {
        let filter = try BSONDocument(json: filter)
        let opts = try BSONDocument(json: #"{"comment":\#(MongoShell.jsonString(tag))}"#)
        return try withCollection(database, collection) { coll in
            try BSON.withReply { reply in
                var error = bson_error_t()
                let count = mongoc_collection_count_documents(coll, filter.pointer, opts.pointer, nil, reply, &error)
                guard count >= 0 else { throw DatabaseError(message: BSON.message(&error)) }
                return Int(count)
            }
        }
    }

    // MARK: Editing

    /// Sets the changed fields of one document, found by its _id, in one updateOne.
    /// Each value keeps the type the field has in that document (see `extendedJSON(from:like:)`).
    public func update(_ relation: Relation, keys: RowKeys, row: Int, changes: [(String, String?)]) throws -> EditOutcome {
        guard !changes.isEmpty else { return EditOutcome(sql: "", command: "UPDATE 0", affected: 0) }
        guard !changes.contains(where: { $0.0 == "_id" }) else {
            throw DatabaseError(message: String(localized: "The _id of a document can't be changed. Insert a copy with the new _id and delete this one."))
        }
        let filter = try idFilter(keys, row: row)
        let types = try fieldTypes(relation, filter: filter, fields: changes.map(\.0))
        let set = try changes.map { name, text in
            MongoShell.jsonString(name) + ":" + (try Self.extendedJSON(from: text, like: types[name] ?? ""))
        }
        let update = try BSONDocument(json: #"{"$set":{"# + set.joined(separator: ",") + "}}")
        let matched = try withCollection(relation.schema, relation.name) { coll in
            try BSON.withReply { reply in
                var error = bson_error_t()
                guard mongoc_collection_update_one(coll, filter.pointer, update.pointer, nil, reply, &error) else {
                    throw DatabaseError(message: BSON.message(&error))
                }
                return BSON.integer(reply, "matchedCount")
            }
        }
        guard matched == 1 else { throw Self.rowChanged }
        let fields = changes.map { "\(Self.shellKey($0.0)): ?" }.joined(separator: ", ")
        return EditOutcome(sql: "db.\(Self.shellName(relation.name)).updateOne({_id: ?}, {$set: {\(fields)}})",
                           command: "UPDATE 1", affected: 1)
    }

    /// Inserts one document with the given fields. An empty _id is left out, so
    /// the driver makes an ObjectId. Values take the types of the collection's
    /// last page when the text reads as that type (else it's read as JSON, falling back to text).
    public func insert(_ relation: Relation, values: [(String, String?)]) throws -> EditOutcome {
        let types = columnTypes[relation.id] ?? [:]
        let fields = values.filter { !($0.0 == "_id" && ($0.1 ?? "").isEmpty) }
        // The types are other documents' (this one is new), so text that doesn't read
        // as the column's type is taken as it reads on its own.
        let members = try fields.map { name, text in
            let value = try (try? Self.extendedJSON(from: text, like: types[name] ?? "")) ?? Self.extendedJSON(from: text, like: "")
            return MongoShell.jsonString(name) + ":" + value
        }
        let doc = try BSONDocument(json: "{" + members.joined(separator: ",") + "}")
        try withCollection(relation.schema, relation.name) { coll in
            try BSON.withReply { reply in
                var error = bson_error_t()
                guard mongoc_collection_insert_one(coll, doc.pointer, nil, reply, &error) else {
                    throw DatabaseError(message: BSON.message(&error))
                }
            }
        }
        let shown = fields.map { "\(Self.shellKey($0.0)): ?" }.joined(separator: ", ")
        return EditOutcome(sql: "db.\(Self.shellName(relation.name)).insertOne({\(shown)})", command: "INSERT 1", affected: 1)
    }

    /// Deletes one document, found by its _id.
    public func delete(_ relation: Relation, keys: RowKeys, row: Int) throws -> EditOutcome {
        let filter = try idFilter(keys, row: row)
        let deleted = try withCollection(relation.schema, relation.name) { coll in
            try BSON.withReply { reply in
                var error = bson_error_t()
                guard mongoc_collection_delete_one(coll, filter.pointer, nil, reply, &error) else {
                    throw DatabaseError(message: BSON.message(&error))
                }
                return BSON.integer(reply, "deletedCount")
            }
        }
        guard deleted == 1 else { throw Self.rowChanged }
        return EditOutcome(sql: "db.\(Self.shellName(relation.name)).deleteOne({_id: ?})", command: "DELETE 1", affected: 1)
    }

    static var rowChanged: DatabaseError {
        DatabaseError(message: String(localized: "The row changed or was deleted since it was loaded. Reload the table and try again."))
    }

    /// {_id: …} from the row's key (canonical extended JSON).
    private func idFilter(_ keys: RowKeys, row: Int) throws -> BSONDocument {
        guard row >= 0, row < keys.values.count, let id = keys.values[row].first ?? nil else { throw Self.rowChanged }
        return try BSONDocument(json: #"{"_id":\#(id)}"#)
    }

    /// The BSON type of each field in the document as it is now ("" for a missing or
    /// null field). Throws the "row changed" message when the document is gone.
    private func fieldTypes(_ relation: Relation, filter: BSONDocument, fields: [String]) throws -> [String: String] {
        let projection = fields.map { MongoShell.jsonString($0) + ":1" }.joined(separator: ",")
        let opts = try BSONDocument(json: #"{"projection":{\#(projection)},"limit":1}"#)
        var types: [String: String] = [:]
        var found = false
        try withCollection(relation.schema, relation.name) { coll in
            try drain(mongoc_collection_find_with_opts(coll, filter.pointer, opts.pointer, nil), limit: 1) { doc in
                found = true
                var iter = bson_iter_t()
                guard bson_iter_init(&iter, doc) else { return }
                while bson_iter_next(&iter) {
                    let type = BSON.typeName(bson_iter_type(&iter))
                    types[String(cString: bson_iter_key(&iter))] = type == "null" ? "" : type
                }
            }
        }
        guard found else { throw Self.rowChanged }
        return types
    }

    /// `users`, or `getCollection("my-things")` when the name isn't a plain identifier.
    static func shellName(_ name: String) -> String {
        name.range(of: "^[A-Za-z_$][A-Za-z0-9_$]*$", options: .regularExpression) != nil
            ? name : "getCollection(\(MongoShell.jsonString(name)))"
    }

    /// `name`, or `"odd key"` in quotes.
    static func shellKey(_ name: String) -> String {
        name.range(of: "^[A-Za-z_$][A-Za-z0-9_$]*$", options: .regularExpression) != nil ? name : MongoShell.jsonString(name)
    }

    // MARK: Values typed in the grid

    /// A value typed in the grid, as extended JSON for a field whose BSON type is
    /// `originalType` (a `Column.type`: "string", "int", "objectId", …; "" or "null"
    /// when unknown). nil is null. A string field stays a string ("42" is text);
    /// int, long, double, decimal, bool, objectId (24 hex digits) and date (ISO 8601)
    /// must read as that type; other types and unknown fields take JSON or mongosh
    /// literals ({a: 1}, [1, 2], ObjectId("…"), {"$date": …}), and an unknown field
    /// keeps text that isn't JSON as a string. Throws "This isn't a valid <type>."
    public static func extendedJSON(from text: String?, like originalType: String) throws -> String {
        guard let text else { return "null" }
        let value = text.trimmingCharacters(in: .whitespacesAndNewlines)
        func invalid() -> DatabaseError { DatabaseError(message: String(localized: "This isn't a valid \(originalType).")) }
        func orInvalid(_ json: String?) throws -> String {
            guard let json else { throw invalid() }
            return json
        }
        /// JSON or a mongosh literal, checked by libbson.
        func literal() -> String? {
            guard let json = try? MongoShell.extendedJSON(value), (try? BSONDocument(json: #"{"v":\#(json)}"#)) != nil else { return nil }
            return json
        }
        /// The literal when it reads as this type (ObjectId("…"), {"$date": …}, …).
        func literal(ofType type: String) -> String? {
            guard let json = literal(), BSON.typeOfValue(json) == type else { return nil }
            return json
        }
        switch originalType {
        case "string", "symbol":
            return MongoShell.jsonString(text)
        case "int":
            if let number = Int32(value) { return #"{"$numberInt":"\#(number)"}"# }
            return try orInvalid(literal(ofType: "int"))
        case "long":
            if let number = Int64(value) { return #"{"$numberLong":"\#(number)"}"# }
            return try orInvalid(literal(ofType: "long"))
        case "double":
            if let number = BSON.double(value) { return #"{"$numberDouble":"\#(BSON.doubleText(number))"}"# }
            return try orInvalid(literal(ofType: "double"))
        case "decimal":
            if BSON.isDecimal(value) { return #"{"$numberDecimal":"\#(value)"}"# }
            return try orInvalid(literal(ofType: "decimal"))
        case "bool":
            switch value.lowercased() {
            case "true": return "true"
            case "false": return "false"
            default: throw invalid()
            }
        case "objectId":
            if value.count == 24, value.allSatisfy(\.isHexDigit) { return #"{"$oid":"\#(value.lowercased())"}"# }
            return try orInvalid(literal(ofType: "objectId"))
        case "date":
            if let ms = MongoShell.isoDateMilliseconds(value) { return MongoShell.dateJSON(ms) }
            return try orInvalid(literal(ofType: "date"))
        case "object", "array":
            return try orInvalid(literal(ofType: originalType))
        case "", "null", "undefined":
            return literal() ?? MongoShell.jsonString(text)
        default:
            return try orInvalid(literal())
        }
    }
}

// MARK: Schema board

extension MongoSession {
    /// Each collection's top-level fields from a sample of up to 100 documents:
    /// the most common type, nullable when a field is sometimes missing or null,
    /// `_id` as the key. MongoDB has no foreign keys; a field named `<name>_id` or
    /// `<name>Id` holding ObjectIds gets a link named "guessed" to a collection
    /// `<name>` or `<name>s` of the same database ("guessed (field)" when a collection
    /// has more than one, so link ids stay unique).
    public func schemaGraph() async throws -> SchemaGraph {
        let relations = try relations()
        return try cancelable { tag in
            var tables: [SchemaGraph.Table] = []
            var objectIdFields: [(table: String, database: String, field: String)] = []
            for relation in relations {
                var stats = FieldStats()
                let pipeline = try BSONDocument(json: #"{"pipeline":[{"$sample":{"size":100}}]}"#)
                let opts = try BSONDocument(json: #"{"batchSize":100,"comment":\#(MongoShell.jsonString(tag))}"#)
                try withCollection(relation.schema, relation.name) { coll in
                    try drain(mongoc_collection_aggregate(coll, MONGOC_QUERY_NONE, pipeline.pointer, opts.pointer, nil), limit: 100) {
                        stats.add($0)
                    }
                }
                let columns = stats.columns
                tables.append(SchemaGraph.Table(schema: relation.schema, name: relation.name, isView: relation.kind == .view,
                                                columns: columns))
                for column in columns where column.type == "objectId" && !column.isPrimaryKey {
                    objectIdFields.append((relation.id, relation.schema, column.name))
                }
            }
            let names = Set(tables.map(\.id))
            var links: [SchemaGraph.Link] = []
            for (table, fields) in Dictionary(grouping: objectIdFields, by: \.table).sorted(by: { $0.key < $1.key }) {
                let found = fields.compactMap { item -> (String, String)? in
                    guard let base = Self.referencedName(item.field) else { return nil }
                    let candidates = [base, base + "s", base.lowercased(), base.lowercased() + "s"].map { "\(item.database).\($0)" }
                    return candidates.first(where: names.contains).map { (item.field, $0) }
                }
                for (field, target) in found {
                    links.append(SchemaGraph.Link(name: found.count == 1 ? "guessed" : "guessed (\(field))", from: table,
                                                  fromColumns: [field], to: target, toColumns: ["_id"]))
                }
            }
            return SchemaGraph(tables: tables, links: links)
        }
    }

    /// "user" for user_id and userId; nil for other names.
    static func referencedName(_ field: String) -> String? {
        if field.hasSuffix("_id"), field.count > 3 { return String(field.dropLast(3)) }
        if field.hasSuffix("Id"), let before = field.dropLast(2).last, before.isLowercase || before.isNumber {
            return String(field.dropLast(2))
        }
        return nil
    }
}

/// Field types seen in sampled documents.
private struct FieldStats {
    var order: [String] = []
    var types: [String: [String: Int]] = [:]
    var present: [String: Int] = [:]
    var documents = 0

    mutating func add(_ doc: UnsafePointer<bson_t>) {
        documents += 1
        var iter = bson_iter_t()
        guard bson_iter_init(&iter, doc) else { return }
        while bson_iter_next(&iter) {
            let key = String(cString: bson_iter_key(&iter))
            if types[key] == nil { order.append(key); types[key] = [:] }
            let type = BSON.typeName(bson_iter_type(&iter))
            if type != "null" { present[key, default: 0] += 1 }
            types[key]?[type, default: 0] += 1
        }
    }

    var columns: [SchemaGraph.Column] {
        var names = order
        if let index = names.firstIndex(of: "_id") { names.insert(names.remove(at: index), at: 0) }
        return names.map { name in
            let counts = (types[name] ?? [:]).filter { $0.key != "null" }
            let type = counts.max { $0.value == $1.value ? $0.key > $1.key : $0.value < $1.value }?.key ?? "null"
            return SchemaGraph.Column(name: name, type: type, isPrimaryKey: name == "_id",
                                      isNullable: (present[name] ?? 0) < documents)
        }
    }
}

// MARK: Command panel

extension MongoSession {
    /// Methods that start a read, and the ones that may follow them in a chain.
    static let readingMethods: Set<String> = ["find", "findOne", "aggregate", "countDocuments", "estimatedDocumentCount",
                                             "distinct", "count"]
    static let cursorMethods: Set<String> = ["sort", "limit", "skip", "projection", "toArray", "pretty", "count", "hint",
                                            "maxTimeMS", "batchSize", "collation", "comment"]
    static let readingDatabaseMethods: Set<String> = ["getCollectionNames", "getCollectionInfos", "getName", "stats"]

    /// One statement that only reads: find, aggregate (without $out or $merge),
    /// counts, distinct, and listing. MongoDB has no read-only session, so
    /// anything else is refused before it reaches the server.
    public func readOnly(_ text: String) throws -> Execution {
        let statements = MongoShell.statements(text)
        guard statements.count == 1 else { throw DatabaseError(message: String(localized: "Send one statement at a time.")) }
        let refused = DatabaseError(message: String(localized: "This statement can change data; read-only queries only find, aggregate, count, and list."))
        switch try MongoShell.parse(statements[0]) {
        case .showDatabases, .showCollections:
            break
        case .use:
            throw refused
        case .database(let method, _):
            guard Self.readingDatabaseMethods.contains(method) else { throw refused }
        case .collection(_, let calls):
            guard let first = calls.first, Self.readingMethods.contains(first.name),
                  calls.dropFirst().allSatisfy({ Self.cursorMethods.contains($0.name) }) else { throw refused }
            if first.name == "aggregate", first.arguments.contains(where: { $0.contains("$out") || $0.contains("$merge") }) {
                throw refused
            }
        }
        return try execute(text)
    }

    /// Runs mongosh-style statements, one after another (see `MongoShell.statements`):
    /// find (with sort, limit, skip, projection), findOne, countDocuments,
    /// estimatedDocumentCount, distinct, aggregate, getIndexes, insertOne, insertMany,
    /// updateOne, updateMany, replaceOne, deleteOne, deleteMany, drop, createIndex;
    /// db.runCommand, db.adminCommand, db.getCollectionNames, db.createCollection,
    /// db.dropDatabase, db.stats, db.getName; show dbs, show collections, use <db>.
    public func execute(_ text: String) throws -> Execution {
        let clock = ContinuousClock()
        let start = clock.now
        let statements = MongoShell.statements(text)
        let results = try cancelable { tag in
            try statements.map { try run(MongoShell.parse($0), statement: $0, tag: tag) }
        }
        return Execution(results: results, duration: clock.now - start)
    }

    private func run(_ parsed: MongoShell.Statement, statement: String, tag: String) throws -> QueryResult {
        switch parsed {
        case .use(let name):
            currentDatabase = name
            return QueryResult(columns: [], rows: [], command: "USE \(name)", totalRows: 0)
        case .showDatabases:
            return try runDatabaseMethod("adminCommand", [#"{listDatabases: 1, authorizedDatabases: true}"#], statement: statement, list: "databases")
        case .showCollections:
            return try runDatabaseMethod("getCollectionNames", [], statement: statement)
        case .database(let method, let arguments):
            return try runDatabaseMethod(method, arguments, statement: statement)
        case .collection(let name, let calls):
            return try runCollectionMethod(name, calls, statement: statement, tag: tag)
        }
    }

    private func runDatabaseMethod(_ method: String, _ arguments: [String], statement: String,
                                   list: String? = nil) throws -> QueryResult {
        func commandResult(_ database: String, _ json: String, tag: String = "OK") throws -> QueryResult {
            var table = DocumentTable(keepIDs: false)
            try Self.command(client, database: database, json) { reply in
                guard let list else { return table.add(reply) }
                var iter = bson_iter_t(), child = bson_iter_t()
                guard bson_iter_init_find(&iter, reply, list), bson_iter_recurse(&iter, &child) else { return }
                while bson_iter_next(&child) {
                    if let doc = BSON.document(&child) { table.add(doc.pointer) }
                }
            }
            return table.result(command: list == nil ? tag : "FIND \(table.rows.count)")
        }
        func document(_ index: Int) throws -> String {
            guard index < arguments.count else { throw MongoShell.unsupported(statement) }
            return try Self.documentJSON(arguments[index])
        }
        switch (method, arguments.count) {
        case ("runCommand", 1):
            return try commandResult(currentDatabase, try commandJSON(arguments[0]))
        case ("adminCommand", 1):
            return try commandResult("admin", try commandJSON(arguments[0]))
        case ("getCollectionNames", 0):
            let names = try collections(in: currentDatabase).map(\.name)
            return QueryResult(columns: [Column(name: "name", type: "string")], rows: names.map { [$0] },
                               command: "FIND \(names.count)", totalRows: names.count)
        case ("getName", 0):
            return QueryResult(columns: [Column(name: "value", type: "string")], rows: [[currentDatabase]],
                               command: "FIND 1", totalRows: 1)
        case ("stats", 0):
            return try commandResult(currentDatabase, #"{"dbStats":1}"#)
        case ("createCollection", 1), ("createCollection", 2):
            let name = try MongoShell.stringArgument(arguments[0])
            var json = #"{"create":\#(MongoShell.jsonString(name))"#
            if arguments.count == 2 { json += "," + (try document(1)).dropFirst() } else { json += "}" }
            _ = try commandResult(currentDatabase, json)
            return QueryResult(columns: [], rows: [], command: "CREATE COLLECTION", totalRows: 0)
        case ("dropDatabase", 0):
            _ = try commandResult(currentDatabase, #"{"dropDatabase":1}"#)
            return QueryResult(columns: [], rows: [], command: "DROP DATABASE", totalRows: 0)
        default:
            throw MongoShell.unsupported(statement)
        }
    }

    /// runCommand's argument: a document, or a command name ("ping" is {ping: 1}).
    private func commandJSON(_ argument: String) throws -> String {
        if let name = try? MongoShell.stringArgument(argument) { return #"{\#(MongoShell.jsonString(name)):1}"# }
        return try Self.documentJSON(argument)
    }

    /// An argument that must be a document, as extended JSON.
    static func documentJSON(_ argument: String) throws -> String {
        let json = try MongoShell.extendedJSON(argument)
        guard json.hasPrefix("{"), BSON.typeOfValue(json) == "object" else {
            throw DatabaseError(message: String(localized: "Expected a document like {name: \"Ana\"}, not \(argument)."))
        }
        return json
    }

    private func runCollectionMethod(_ name: String, _ calls: [MongoShell.Call], statement: String,
                                     tag: String) throws -> QueryResult {
        let database = currentDatabase
        let main = calls[0]
        let modifiers = Array(calls.dropFirst()).filter { !["pretty", "toArray"].contains($0.name) }
        let args = main.arguments
        func document(_ index: Int, default fallback: String = "{}") throws -> String {
            index < args.count ? try Self.documentJSON(args[index]) : fallback
        }
        func noModifiers() throws { guard modifiers.isEmpty else { throw MongoShell.unsupported(statement) } }
        func written(_ verb: String, _ count: Int) -> QueryResult {
            QueryResult(columns: [], rows: [], command: "\(verb) \(count)", totalRows: count)
        }
        let commentJSON = #""comment":\#(MongoShell.jsonString(tag))"#

        switch main.name {
        case "find", "findOne":
            guard args.count <= 2 else { throw MongoShell.unsupported(statement) }
            var options = [#""batchSize":\#(Self.batchSize)"#, commentJSON]
            if args.count == 2 { options.append(#""projection":\#(try document(1))"#) }
            if main.name == "findOne" { options.append(#""limit":1"#) }
            let filter = try document(0)
            var counting = false
            for call in modifiers {
                switch (call.name, call.arguments.count) {
                case ("sort", 1): options.append(#""sort":\#(try Self.documentJSON(call.arguments[0]))"#)
                case ("projection", 1): options.append(#""projection":\#(try Self.documentJSON(call.arguments[0]))"#)
                case ("hint", 1): options.append(#""hint":\#(try MongoShell.extendedJSON(call.arguments[0]))"#)
                case ("limit", 1) where main.name == "find": options.append(#""limit":\#(try Self.integer(call.arguments[0]))"#)
                case ("skip", 1): options.append(#""skip":\#(try Self.integer(call.arguments[0]))"#)
                case ("count", 0), ("countDocuments", 0): counting = true
                default: throw MongoShell.unsupported(statement)
                }
            }
            if counting {
                return scalar(try countDocuments(database, name, filter: filter, tag: tag), type: "long", command: "COUNT")
            }
            let filterDoc = try BSONDocument(json: filter)
            let opts = try BSONDocument(json: "{" + options.joined(separator: ",") + "}")
            return try documents(verb: "FIND", in: (database, name)) { coll in
                mongoc_collection_find_with_opts(coll, filterDoc.pointer, opts.pointer, nil)
            }

        case "aggregate":
            try noModifiers()
            guard (1...2).contains(args.count) else { throw MongoShell.unsupported(statement) }
            let stages = try MongoShell.extendedJSON(args[0])
            guard stages.hasPrefix("[") else {
                throw DatabaseError(message: String(localized: "aggregate takes a list of stages, like [{$match: {…}}]."))
            }
            let pipeline = try BSONDocument(json: #"{"pipeline":\#(stages)}"#)
            let opts = try BSONDocument(json: try document(1))
            if !opts.has("batchSize") { opts.append("batchSize", Int64(Self.batchSize)) }
            if !opts.has("comment") { opts.append("comment", tag) }
            return try documents(verb: "AGGREGATE", in: (database, name)) { coll in
                mongoc_collection_aggregate(coll, MONGOC_QUERY_NONE, pipeline.pointer, opts.pointer, nil)
            }

        case "countDocuments", "count":
            try noModifiers()
            guard args.count <= 1 else { throw MongoShell.unsupported(statement) }
            return scalar(try countDocuments(database, name, filter: try document(0), tag: tag), type: "long", command: "COUNT")

        case "estimatedDocumentCount":
            try noModifiers()
            guard args.isEmpty else { throw MongoShell.unsupported(statement) }
            let count = try withCollection(database, name) { coll in
                try BSON.withReply { reply in
                    var error = bson_error_t()
                    let count = mongoc_collection_estimated_document_count(coll, nil, nil, reply, &error)
                    guard count >= 0 else { throw DatabaseError(message: BSON.message(&error)) }
                    return Int(count)
                }
            }
            return scalar(count, type: "long", command: "COUNT")

        case "distinct":
            try noModifiers()
            guard (1...2).contains(args.count) else { throw MongoShell.unsupported(statement) }
            let field = try MongoShell.stringArgument(args[0])
            let command = #"{"distinct":\#(MongoShell.jsonString(name)),"key":\#(MongoShell.jsonString(field)),"query":\#(try document(1)),\#(commentJSON)}"#
            var rows: [[String?]] = []
            var type: String?
            var truncated = false
            try Self.command(client, database: database, command) { reply in
                var iter = bson_iter_t(), child = bson_iter_t()
                guard bson_iter_init_find(&iter, reply, "values"), bson_iter_recurse(&iter, &child) else { return }
                while bson_iter_next(&child) {
                    guard rows.count < QueryResult.rowLimit else { truncated = true; break }
                    if type == nil, bson_iter_type(&child) != BSON_TYPE_NULL { type = BSON.typeName(bson_iter_type(&child)) }
                    rows.append([BSON.cell(&child)])
                }
            }
            return QueryResult(columns: [Column(name: "value", type: type ?? "")], rows: rows,
                               command: "DISTINCT \(rows.count)", truncated: truncated, totalRows: rows.count)

        case "getIndexes":
            try noModifiers()
            return try documents(verb: "FIND", in: (database, name)) { coll in mongoc_collection_find_indexes_with_opts(coll, nil) }

        case "insertOne":
            try noModifiers()
            guard (1...2).contains(args.count) else { throw MongoShell.unsupported(statement) }
            let doc = try BSONDocument(json: try document(0))
            let opts = try BSONDocument(json: try document(1))
            try write(database, name) { coll, reply, error in mongoc_collection_insert_one(coll, doc.pointer, opts.pointer, reply, &error) }
            return written("INSERT", 1)

        case "insertMany":
            try noModifiers()
            guard (1...2).contains(args.count) else { throw MongoShell.unsupported(statement) }
            let list = try MongoShell.extendedJSON(args[0])
            guard list.hasPrefix("[") else {
                throw DatabaseError(message: String(localized: "insertMany takes a list of documents, like [{a: 1}, {a: 2}]."))
            }
            let docs = try BSON.documents(inArray: list)
            guard !docs.isEmpty else { return written("INSERT", 0) }
            let opts = try BSONDocument(json: try document(1))
            var pointers: [UnsafePointer<bson_t>?] = docs.map { UnsafePointer($0.pointer) }
            let total = pointers.count
            let inserted = try write(database, name, count: "insertedCount") { coll, reply, error in
                mongoc_collection_insert_many(coll, &pointers, total, opts.pointer, reply, &error)
            }
            return written("INSERT", inserted)

        case "updateOne", "updateMany", "replaceOne":
            try noModifiers()
            guard (2...3).contains(args.count) else { throw MongoShell.unsupported(statement) }
            let filter = try BSONDocument(json: try document(0))
            let change = try MongoShell.extendedJSON(args[1])
            let update: BSONDocument
            if change.hasPrefix("["), main.name != "replaceOne" {
                // An update pipeline: libmongoc takes it as a document keyed "0", "1", …
                update = try BSON.arrayDocument(change)
            } else {
                update = try BSONDocument(json: try Self.documentJSON(args[1]))
            }
            let opts = try BSONDocument(json: try document(2))
            let modified = try write(database, name, count: "modifiedCount") { coll, reply, error in
                switch main.name {
                case "updateOne": mongoc_collection_update_one(coll, filter.pointer, update.pointer, opts.pointer, reply, &error)
                case "updateMany": mongoc_collection_update_many(coll, filter.pointer, update.pointer, opts.pointer, reply, &error)
                default: mongoc_collection_replace_one(coll, filter.pointer, update.pointer, opts.pointer, reply, &error)
                }
            }
            return written("UPDATE", modified)

        case "deleteOne", "deleteMany":
            try noModifiers()
            guard (1...2).contains(args.count) else { throw MongoShell.unsupported(statement) }
            let filter = try BSONDocument(json: try document(0))
            let opts = try BSONDocument(json: try document(1))
            let deleted = try write(database, name, count: "deletedCount") { coll, reply, error in
                main.name == "deleteOne"
                    ? mongoc_collection_delete_one(coll, filter.pointer, opts.pointer, reply, &error)
                    : mongoc_collection_delete_many(coll, filter.pointer, opts.pointer, reply, &error)
            }
            return written("DELETE", deleted)

        case "drop":
            try noModifiers()
            guard args.isEmpty else { throw MongoShell.unsupported(statement) }
            try withCollection(database, name) { coll in
                var error = bson_error_t()
                guard mongoc_collection_drop_with_opts(coll, nil, &error) else {
                    // Dropping a collection that isn't there is not an error in mongosh.
                    if error.code == 26 { return }
                    throw DatabaseError(message: BSON.message(&error))
                }
            }
            return QueryResult(columns: [], rows: [], command: "DROP", totalRows: 0)

        case "createIndex":
            try noModifiers()
            guard (1...2).contains(args.count) else { throw MongoShell.unsupported(statement) }
            let keysJSON = try document(0)
            let keys = try BSONDocument(json: keysJSON)
            let options = try document(1)
            let optionDoc = try BSONDocument(json: options)
            var indexName = Self.indexName(keys)
            var index = #"{"key":\#(keysJSON)"#
            if optionDoc.has("name") {
                var iter = bson_iter_t()
                if bson_iter_init_find(&iter, optionDoc.pointer, "name"), let given = BSON.cell(&iter) { indexName = given }
            } else {
                index += #","name":\#(MongoShell.jsonString(indexName))"#
            }
            index += options == "{}" ? "}" : "," + options.dropFirst()
            let command = #"{"createIndexes":\#(MongoShell.jsonString(name)),"indexes":[\#(index)]}"#
            try Self.command(client, database: database, command) { _ in }
            return QueryResult(columns: [Column(name: "value", type: "string")], rows: [[indexName]],
                               command: "CREATE INDEX", totalRows: 1)

        default:
            throw MongoShell.unsupported(statement)
        }
    }

    /// mongosh's index name: {a: 1, b: -1} is "a_1_b_-1".
    static func indexName(_ keys: BSONDocument) -> String {
        var parts: [String] = []
        var iter = bson_iter_t()
        guard bson_iter_init(&iter, keys.pointer) else { return "" }
        while bson_iter_next(&iter) {
            parts.append(String(cString: bson_iter_key(&iter)))
            parts.append(BSON.cell(&iter) ?? "null")
        }
        return parts.joined(separator: "_")
    }

    /// limit(5), skip(NumberInt(5)): a whole number.
    static func integer(_ argument: String) throws -> Int64 {
        let json = try MongoShell.extendedJSON(argument)
        if let value = Int64(json) ?? Double(json).flatMap({ Int64(exactly: $0) }) { return value }
        let doc = try BSONDocument(json: #"{"v":\#(json)}"#)
        var iter = bson_iter_t()
        if bson_iter_init_find(&iter, doc.pointer, "v"), BSON.isNumber(bson_iter_type(&iter)) { return bson_iter_as_int64(&iter) }
        throw DatabaseError(message: String(localized: "Expected a number, not \(argument)."))
    }

    private func scalar(_ value: Int, type: String, command: String) -> QueryResult {
        QueryResult(columns: [Column(name: "value", type: type)], rows: [[String(value)]], command: command, totalRows: 1)
    }

    /// The documents of a cursor as a table, cut at `QueryResult.rowLimit`.
    private func documents(verb: String, in namespace: (database: String, collection: String),
                           _ open: (OpaquePointer) -> OpaquePointer?) throws -> QueryResult {
        var table = DocumentTable(keepIDs: false)
        let more = try withCollection(namespace.database, namespace.collection) { coll in
            try drain(open(coll), limit: QueryResult.rowLimit) { table.add($0) }
        }
        var result = table.result(command: "\(verb) \(table.rows.count)")
        result.truncated = more
        return result
    }

    /// Runs a write; returns the reply's `count` field (0 when not asked).
    @discardableResult
    private func write(_ database: String, _ collection: String, count: String? = nil,
                       _ body: (OpaquePointer, UnsafeMutablePointer<bson_t>, inout bson_error_t) -> Bool) throws -> Int {
        try withCollection(database, collection) { coll in
            try BSON.withReply { reply in
                var error = bson_error_t()
                guard body(coll, reply, &error) else { throw DatabaseError(message: BSON.message(&error)) }
                return count.map { BSON.integer(reply, $0) } ?? 0
            }
        }
    }
}

// MARK: Canceling

/// Whether an operation runs and whether a cancel came for it, shared with
/// `cancel()`; kills the running operation from its own short connection.
private final class MongoCanceller: Sendable {
    private struct State {
        var running = false
        var requested = false
        var generation = 0
    }

    private let state = Mutex(State())
    private let uri: String
    private let session = UUID().uuidString

    init(uri: String) { self.uri = uri }

    /// Starts an operation; returns the comment its reads carry.
    func begin() -> String {
        let generation = state.withLock { state in
            state.generation += 1
            state.running = true
            state.requested = false
            return state.generation
        }
        return tag(generation)
    }

    /// Ends an operation; true when it was canceled.
    func end() -> Bool {
        state.withLock { state in
            defer { state.running = false; state.requested = false }
            return state.requested
        }
    }

    var requested: Bool { state.withLock { $0.requested } }

    private func tag(_ generation: Int) -> String { "dbLens \(session) #\(generation)" }

    /// Marks the running operation canceled and kills it on the server, from
    /// another thread. Does nothing when nothing runs.
    func cancel() {
        let generation: Int? = state.withLock { state in
            guard state.running, !state.requested else { return nil }
            state.requested = true
            return state.generation
        }
        guard let generation else { return }
        Thread.detachNewThread { [self] in kill(generation) }
    }

    private func stillRunning(_ generation: Int) -> Bool {
        state.withLock { $0.running && $0.generation == generation }
    }

    /// Looks for the operation by its comment and kills it; tries for a few
    /// seconds, as the operation may not have reached the server yet.
    private func kill(_ generation: Int) {
        guard let client = try? MongoSession.openClient(uri) else { return }
        defer { mongoc_client_destroy(client) }
        let comment = MongoShell.jsonString(tag(generation))
        let find = #"{"aggregate":1,"pipeline":[{"$currentOp":{"allUsers":false,"idleCursors":false}},"#
            + #"{"$match":{"$or":[{"command.comment":\#(comment)},{"cursor.originatingCommand.comment":\#(comment)}]}},"#
            + #"{"$project":{"opid":1}}],"cursor":{}}"#
        for _ in 0..<20 where stillRunning(generation) {
            var opids: [String] = []
            try? MongoSession.command(client, database: "admin", find) { reply in
                var root = bson_iter_t(), batch = bson_iter_t(), iter = bson_iter_t()
                guard bson_iter_init(&root, reply), bson_iter_find_descendant(&root, "cursor.firstBatch", &batch),
                      bson_iter_recurse(&batch, &iter) else { return }
                while bson_iter_next(&iter) {
                    guard let op = BSON.document(&iter) else { continue }
                    var field = bson_iter_t()
                    if bson_iter_init_find(&field, op.pointer, "opid"), let json = BSON.valueJSON(&field, canonical: true) {
                        opids.append(json)
                    }
                }
            }
            for opid in opids {
                try? MongoSession.command(client, database: "admin", #"{"killOp":1,"op":\#(opid)}"#) { _ in }
            }
            if !opids.isEmpty { return }
            Thread.sleep(forTimeInterval: 0.15)
        }
    }
}

// MARK: BSON

/// A BSON document libbson owns, destroyed with this object.
final class BSONDocument {
    let pointer: UnsafeMutablePointer<bson_t>

    init() { pointer = bson_new() }

    /// Takes ownership of a document made by bson_new…
    init(owning pointer: UnsafeMutablePointer<bson_t>) { self.pointer = pointer }

    /// Reads extended JSON (canonical or relaxed), or throws libbson's message.
    /// libbson reads every "$type" key as legacy binary ({"$binary": …, "$type": "00"}),
    /// which breaks the $type query operator ({a: {$type: "date"}}), so in compact
    /// JSON those keys are read under another name and renamed back.
    init(json: String) throws {
        let shielded = json.contains(#""$type":"#) && !json.contains(#""$binary":""#)
        let text = shielded ? json.replacingOccurrences(of: #"(?<=[{,])"\$type":"#, with: NSRegularExpression.escapedTemplate(for: "\"\(Self.typeKey)\":"),
                                                        options: .regularExpression) : json
        var error = bson_error_t()
        let bytes = Array(text.utf8)
        guard let parsed = bytes.withUnsafeBufferPointer({ bson_new_from_json($0.baseAddress, $0.count, &error) }) else {
            throw DatabaseError(message: BSON.message(&error))
        }
        guard shielded else {
            pointer = parsed
            return
        }
        pointer = bson_new()
        Self.copy(parsed, into: pointer)
        bson_destroy(parsed)
    }

    /// What "$type" keys are called while libbson reads them.
    private static let typeKey = "$dbLens.type"

    /// Copies a document, giving shielded "$type" keys their name back.
    private static func copy(_ source: UnsafePointer<bson_t>, into target: UnsafeMutablePointer<bson_t>) {
        var iter = bson_iter_t()
        guard bson_iter_init(&iter, source) else { return }
        while bson_iter_next(&iter) {
            let key = String(cString: bson_iter_key(&iter))
            let name = key == typeKey ? "$type" : key
            let type = bson_iter_type(&iter)
            if type == BSON_TYPE_DOCUMENT || type == BSON_TYPE_ARRAY, let child = BSON.document(&iter) {
                let copied = BSONDocument()
                copy(child.pointer, into: copied.pointer)
                if type == BSON_TYPE_DOCUMENT {
                    bson_append_document(target, name, -1, copied.pointer)
                } else {
                    bson_append_array(target, name, -1, copied.pointer)
                }
            } else {
                bson_append_iter(target, name, -1, &iter)
            }
        }
    }

    deinit { bson_destroy(pointer) }

    func has(_ key: String) -> Bool { bson_has_field(pointer, key) }
    func append(_ key: String, _ value: Int64) { bson_append_int64(pointer, key, -1, value) }
    func append(_ key: String, _ value: String) { bson_append_utf8(pointer, key, -1, value, -1) }
}

/// Reading BSON values for the grid.
enum BSON {
    /// libbson's or libmongoc's message, as is.
    static func message(_ error: inout bson_error_t) -> String {
        let text = withUnsafeBytes(of: &error.message) { raw in
            String(decoding: raw.prefix { $0 != 0 }, as: UTF8.self)
        }
        return text.isEmpty ? String(localized: "The server didn't say what went wrong.") : text
    }

    /// Calls `body` with an empty reply document that is destroyed afterwards.
    static func withReply<T>(_ body: (UnsafeMutablePointer<bson_t>) throws -> T) rethrows -> T {
        let reply = UnsafeMutablePointer<bson_t>.allocate(capacity: 1)
        bson_init(reply)
        defer {
            bson_destroy(reply)
            reply.deallocate()
        }
        return try body(reply)
    }

    /// A whole-number field of a reply (matchedCount, deletedCount, …), 0 when missing.
    static func integer(_ doc: UnsafePointer<bson_t>, _ key: String) -> Int {
        var iter = bson_iter_t()
        guard bson_iter_init_find(&iter, doc, key), isNumber(bson_iter_type(&iter)) else { return 0 }
        return Int(bson_iter_as_int64(&iter))
    }

    static func isNumber(_ type: bson_type_t) -> Bool {
        type == BSON_TYPE_INT32 || type == BSON_TYPE_INT64 || type == BSON_TYPE_DOUBLE
    }

    /// The document or array at the iterator, as its own document.
    static func document(_ iter: inout bson_iter_t) -> BSONDocument? {
        let type = bson_iter_type(&iter)
        var length: UInt32 = 0
        var data: UnsafePointer<UInt8>?
        if type == BSON_TYPE_DOCUMENT {
            bson_iter_document(&iter, &length, &data)
        } else if type == BSON_TYPE_ARRAY {
            bson_iter_array(&iter, &length, &data)
        } else {
            return nil
        }
        guard let data, let copy = bson_new_from_data(data, Int(length)) else { return nil }
        return BSONDocument(owning: copy)
    }

    /// The documents of a JSON array; throws when an item isn't a document.
    static func documents(inArray json: String) throws -> [BSONDocument] {
        let wrapper = try BSONDocument(json: #"{"v":\#(json)}"#)
        var iter = bson_iter_t(), child = bson_iter_t()
        guard bson_iter_init_find(&iter, wrapper.pointer, "v"), bson_iter_recurse(&iter, &child) else { return [] }
        var docs: [BSONDocument] = []
        while bson_iter_next(&child) {
            guard bson_iter_type(&child) == BSON_TYPE_DOCUMENT, let doc = document(&child) else {
                throw DatabaseError(message: String(localized: "Each item of the list must be a document, like {name: \"Ana\"}."))
            }
            docs.append(doc)
        }
        return docs
    }

    /// A JSON array as a document keyed "0", "1", …
    static func arrayDocument(_ json: String) throws -> BSONDocument {
        let wrapper = try BSONDocument(json: #"{"v":\#(json)}"#)
        var iter = bson_iter_t()
        guard bson_iter_init_find(&iter, wrapper.pointer, "v"), let doc = document(&iter) else {
            throw DatabaseError(message: String(localized: "Expected a list."))
        }
        return doc
    }

    /// The BSON type name of one extended JSON value ("int", "objectId", …), or nil when it doesn't read.
    static func typeOfValue(_ json: String) -> String? {
        guard let doc = try? BSONDocument(json: #"{"v":\#(json)}"#) else { return nil }
        var iter = bson_iter_t()
        guard bson_iter_init_find(&iter, doc.pointer, "v") else { return nil }
        return typeName(bson_iter_type(&iter))
    }

    /// mongosh's names for BSON types, as `$type` takes them.
    static func typeName(_ type: bson_type_t) -> String {
        switch type {
        case BSON_TYPE_DOUBLE: "double"
        case BSON_TYPE_UTF8: "string"
        case BSON_TYPE_DOCUMENT: "object"
        case BSON_TYPE_ARRAY: "array"
        case BSON_TYPE_BINARY: "binData"
        case BSON_TYPE_UNDEFINED: "undefined"
        case BSON_TYPE_OID: "objectId"
        case BSON_TYPE_BOOL: "bool"
        case BSON_TYPE_DATE_TIME: "date"
        case BSON_TYPE_NULL: "null"
        case BSON_TYPE_REGEX: "regex"
        case BSON_TYPE_DBPOINTER: "dbPointer"
        case BSON_TYPE_CODE: "javascript"
        case BSON_TYPE_SYMBOL: "symbol"
        case BSON_TYPE_CODEWSCOPE: "javascriptWithScope"
        case BSON_TYPE_INT32: "int"
        case BSON_TYPE_TIMESTAMP: "timestamp"
        case BSON_TYPE_INT64: "long"
        case BSON_TYPE_DECIMAL128: "decimal"
        case BSON_TYPE_MINKEY: "minKey"
        case BSON_TYPE_MAXKEY: "maxKey"
        default: "unknown"
        }
    }

    /// A value as the grid shows it: text as is, numbers plain, true/false, null
    /// as nil, ObjectIds as 24 hex digits, dates in ISO 8601 (UTC, milliseconds),
    /// everything else (documents, arrays, binary, …) as compact relaxed extended JSON.
    static func cell(_ iter: inout bson_iter_t) -> String? {
        switch bson_iter_type(&iter) {
        case BSON_TYPE_UTF8, BSON_TYPE_SYMBOL:
            var length: UInt32 = 0
            guard let text = bson_iter_type(&iter) == BSON_TYPE_UTF8 ? bson_iter_utf8(&iter, &length)
                    : bson_iter_symbol(&iter, &length) else { return "" }
            return String(decoding: UnsafeRawBufferPointer(start: text, count: Int(length)), as: UTF8.self)
        case BSON_TYPE_INT32: return String(bson_iter_int32(&iter))
        case BSON_TYPE_INT64: return String(bson_iter_int64(&iter))
        case BSON_TYPE_DOUBLE: return doubleText(bson_iter_double(&iter))
        case BSON_TYPE_DECIMAL128:
            var value = bson_decimal128_t()
            guard bson_iter_decimal128(&iter, &value) else { return nil }
            var buffer = [CChar](repeating: 0, count: 64)
            bson_decimal128_to_string(&value, &buffer)
            return String(decoding: buffer.prefix { $0 != 0 }.map { UInt8(bitPattern: $0) }, as: UTF8.self)
        case BSON_TYPE_BOOL: return bson_iter_bool(&iter) ? "true" : "false"
        case BSON_TYPE_NULL, BSON_TYPE_UNDEFINED: return nil
        case BSON_TYPE_OID:
            var buffer = [CChar](repeating: 0, count: 25)
            bson_oid_to_string(bson_iter_oid(&iter), &buffer)
            return String(decoding: buffer.prefix { $0 != 0 }.map { UInt8(bitPattern: $0) }, as: UTF8.self)
        case BSON_TYPE_DATE_TIME:
            let ms = bson_iter_date_time(&iter)
            return MongoShell.isoDate(milliseconds: ms) ?? valueJSON(&iter, canonical: false)
        default:
            return valueJSON(&iter, canonical: false)
        }
    }

    /// One value as compact extended JSON: relaxed for showing, canonical for
    /// finding the same value again (an _id).
    static func valueJSON(_ iter: inout bson_iter_t, canonical: Bool) -> String? {
        let wrapper = BSONDocument()
        guard bson_append_iter(wrapper.pointer, "v", 1, &iter) else { return nil }
        var length = 0
        guard let json = canonical ? bson_as_canonical_extended_json(wrapper.pointer, &length)
                : bson_as_relaxed_extended_json(wrapper.pointer, &length) else { return nil }
        defer { bson_free(json) }
        let compact = compactJSON(UnsafeRawBufferPointer(start: json, count: length))
        // {"v":…}
        guard compact.hasPrefix("{\"v\":"), compact.hasSuffix("}") else { return compact }
        return String(compact.dropFirst(5).dropLast())
    }

    /// JSON without the spaces outside strings.
    static func compactJSON(_ bytes: UnsafeRawBufferPointer) -> String {
        var out: [UInt8] = []
        out.reserveCapacity(bytes.count)
        var inString = false
        var escaped = false
        for byte in bytes {
            if inString {
                out.append(byte)
                if escaped { escaped = false } else if byte == UInt8(ascii: "\\") { escaped = true } else if byte == UInt8(ascii: "\"") { inString = false }
                continue
            }
            switch byte {
            case UInt8(ascii: " "), UInt8(ascii: "\n"), UInt8(ascii: "\t"), UInt8(ascii: "\r"): continue
            case UInt8(ascii: "\""): inString = true
            default: break
            }
            out.append(byte)
        }
        return String(decoding: out, as: UTF8.self)
    }

    /// 1.5, 1.0, 1e+20, NaN, Infinity, -Infinity.
    static func doubleText(_ value: Double) -> String {
        if value.isNaN { return "NaN" }
        if value.isInfinite { return value < 0 ? "-Infinity" : "Infinity" }
        return "\(value)"
    }

    /// A double as typed: 1.5, 3, 1e3, NaN, Infinity, -Infinity.
    static func double(_ text: String) -> Double? {
        switch text {
        case "NaN": return .nan
        case "Infinity", "+Infinity": return .infinity
        case "-Infinity": return -.infinity
        default:
            guard text.range(of: #"^[+-]?(\d+\.?\d*|\.\d+)([eE][+-]?\d+)?$"#, options: .regularExpression) != nil else { return nil }
            return Double(text)
        }
    }

    static func isDecimal(_ text: String) -> Bool {
        var value = bson_decimal128_t()
        return !text.isEmpty && bson_decimal128_from_string(text, &value)
    }

    static func newObjectId() -> String {
        var oid = bson_oid_t()
        bson_oid_init(&oid, nil)
        var buffer = [CChar](repeating: 0, count: 25)
        bson_oid_to_string(&oid, &buffer)
        return String(decoding: buffer.prefix { $0 != 0 }.map { UInt8(bitPattern: $0) }, as: UTF8.self)
    }
}

/// Documents as a table: `_id` first, then every top-level field in order of
/// first appearance; a column's type is the BSON type of its first non-null value.
struct DocumentTable {
    private var names: [String] = []
    private var index: [String: Int] = [:]
    private var types: [String?] = []
    private(set) var rows: [[String?]] = []
    private var ids: [String?] = []
    let keepIDs: Bool

    init(keepIDs: Bool) { self.keepIDs = keepIDs }

    /// Each row's _id in canonical extended JSON (when `keepIDs`).
    var orderedIDs: [String?] { ids }

    mutating func add(_ doc: UnsafePointer<bson_t>) {
        var row = [String?](repeating: nil, count: names.count)
        var id: String?
        var iter = bson_iter_t()
        guard bson_iter_init(&iter, doc) else { return }
        while bson_iter_next(&iter) {
            let key = String(cString: bson_iter_key(&iter))
            let column: Int
            if let existing = index[key] {
                column = existing
            } else {
                column = names.count
                index[key] = column
                names.append(key)
                types.append(nil)
                row.append(nil)
            }
            let type = BSON.typeName(bson_iter_type(&iter))
            if types[column] == nil || (types[column] == "null" && type != "null") { types[column] = type }
            row[column] = BSON.cell(&iter)
            if keepIDs, key == "_id" { id = BSON.valueJSON(&iter, canonical: true) }
        }
        rows.append(row)
        if keepIDs { ids.append(id) }
    }

    func result(command: String) -> QueryResult {
        var order = Array(names.indices)
        if let id = index["_id"], id != 0 { order.insert(order.remove(at: id), at: 0) }
        let columns = order.map { Column(name: names[$0], type: types[$0] ?? "") }
        let table = rows.map { row in order.map { $0 < row.count ? row[$0] : nil } }
        return QueryResult(columns: columns, rows: table, command: command, totalRows: table.count)
    }
}

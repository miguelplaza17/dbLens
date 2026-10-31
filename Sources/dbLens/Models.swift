import AppKit
import Foundation
import SwiftData
import DBLensKit

/// A saved database. The URL holds the password, so it lives in connections.json
/// (Secrets.swift), keyed by `uuid`; only the name is in the store.
@Model
final class SavedConnection {
    var name: String
    var createdAt: Date = Date.now
    var lastUsed: Date?
    var uuid: UUID = UUID()
    /// Through SSH when sshHost is set: a host, an IP, or a ~/.ssh/config alias.
    var sshHost: String = ""
    /// 0 uses ~/.ssh/config or 22.
    var sshPort: Int = 0
    var sshUser: String = ""
    var sshKey: String = ""
    /// Engine.rawValue: which kind of database, for the sidebar icon.
    var engine: String = "postgres"

    init(name: String) {
        self.name = name
    }

    @MainActor var url: String? { Secrets.url(for: uuid) }

    var ssh: SSHTunnel.Config? {
        get {
            sshHost.isEmpty ? nil : SSHTunnel.Config(host: sshHost, port: sshPort > 0 ? sshPort : nil,
                                                     user: sshUser, keyPath: sshKey)
        }
        set {
            sshHost = newValue?.host ?? ""
            sshPort = newValue?.port ?? 0
            sshUser = newValue?.user ?? ""
            sshKey = newValue?.keyPath ?? ""
        }
    }
}

/// What the center grid shows. Snapshot mode walks through every case.
enum Shown: String, CaseIterable {
    case table, query, schema
}

/// One line of the SQL log: a run, an edit, or an error.
/// A statement run in the SQL panel, kept per connection for the History list.
struct HistoryEntry: Codable, Identifiable, Hashable {
    var id = UUID()
    var sql: String
    var date = Date.now
    var failed = false
}

struct LogEntry: Identifiable {
    let id = UUID()
    let date = Date.now
    let sql: String
    let message: String
    let isError: Bool

    /// The entry as plain text, for Copy Log.
    var copyText: String {
        let time = date.formatted(.dateTime.hour().minute().second())
        return "\(time) \(isError ? String(localized: "ERROR") : "OK") \(message)" + (sql.isEmpty ? "" : "\n\n\(sql)")
    }
}

/// The row being edited: an index into the table page, or nil for a new row.
struct RowEditTarget: Identifiable {
    let id = UUID()
    let row: Int?
}

/// App-wide state that menus, toolbars, and views share.
@Observable
final class AppModel {
    static let pageSizes = [50, 100, 200, 500, 1000]
    /// Rows per page in the table grid, remembered between launches.
    var pageSize = UserDefaults.standard.object(forKey: "grid.pageSize") as? Int ?? 200 {
        didSet { if !Store.isOverridden { UserDefaults.standard.set(pageSize, forKey: "grid.pageSize") } }
    }
    /// Text the table grid is filtered by, on the server, across every page.
    private(set) var tableSearch = ""
    /// The column the table is ordered by (on the server), from a header click.
    private(set) var tableSort: ColumnSort?
    /// The column the query result is ordered by, on screen.
    private(set) var querySort: ColumnSort?
    @ObservationIgnored private var searchTask: Task<Void, Never>?

    let container: ModelContainer
    var shown: Shown = .table { didSet { noteMove() } }
    var showingNewConnection = false
    /// Text the next New Connection sheet starts with (snapshots only).
    var connectionDraft: String?
    var editing: SavedConnection?
    var rowEdit: RowEditTarget?
    /// The SQL panel at the bottom of the window, shown or folded away.
    var showSQLPanel = UserDefaults.standard.object(forKey: "sqlPanel.shown") as? Bool ?? true {
        didSet { if !Store.isOverridden { UserDefaults.standard.set(showSQLPanel, forKey: "sqlPanel.shown") } }
    }

    // The open connection.
    private(set) var session: (any DatabaseSession)?
    private(set) var activeID: UUID? { didSet { noteMove() } }
    private(set) var connectingID: UUID?
    var connectError: String?
    private(set) var relations: [Relation] = []

    // Table rows.
    private(set) var selectedRelation: Relation.ID? { didSet { noteMove() } }
    private(set) var tableResult: QueryResult?
    private(set) var tableCount: Int?
    private(set) var tableError: String?
    private(set) var tableLoading = false
    private(set) var offset = 0 { didSet { noteMove() } }
    /// Changes with every load, so the grid rebuilds its columns.
    private(set) var tableVersion = 0
    private var tableShape = ""
    /// The table the grid's rows came from. Edits go there, never to a table
    /// picked since and still loading.
    private var loadedRelation: Relation?

    // SQL.
    var sql = "select now();"
    /// The text selected in the editor; when not empty, Run runs only this.
    var sqlSelection = ""
    private(set) var execution: Execution?
    private(set) var running = false
    private(set) var runVersion = 0 { didSet { noteMove() } }
    /// The last run, edit, or connection; each new one replaces it.
    private(set) var lastLog: LogEntry?
    /// What was run on the open connection, newest first (at most 100, saved between launches).
    private(set) var sqlHistory: [HistoryEntry] = []

    /// Settings reopens on the last tab. Test and snapshot runs leave it alone.
    var settingsTab: SettingsTab = SettingsTab(rawValue: UserDefaults.standard.string(forKey: "settings.tab") ?? "") ?? .general {
        didSet { if !Store.isOverridden { UserDefaults.standard.set(settingsTab.rawValue, forKey: "settings.tab") } }
    }

    init(container: ModelContainer) {
        self.container = container
    }

    var context: ModelContext { container.mainContext }

    var activeConnection: SavedConnection? {
        guard let activeID else { return nil }
        return try? context.fetch(FetchDescriptor<SavedConnection>(predicate: #Predicate { $0.uuid == activeID })).first
    }

    var relation: Relation? { relations.first { $0.id == selectedRelation } }

    /// The grid on screen: the table page or the last query result.
    var shownResult: QueryResult? {
        shown == .table ? tableResult : sortedQueryResult
    }

    /// The query result in the order its header asks for. Query results hold
    /// what the statement returned, so they're sorted here, not on the server.
    private var sortedQueryResult: QueryResult? {
        guard var result = execution?.main, let sort = querySort,
              let column = result.columns.firstIndex(where: { $0.name == sort.column }) else { return execution?.main }
        result.rows.sort { ColumnSort.precedes($0[column], $1[column], ascending: sort.ascending) }
        return result
    }

    /// Names the SQL editor suggests: tables (with their schema when there are
    /// several) and the columns the schema board read.
    var editorNames: EditorNames {
        let tables = relations.map(\.name)
        let columns = Dictionary((schemaGraph?.tables ?? []).map { ($0.name.lowercased(), $0.columns.map(\.name)) },
                                 uniquingKeysWith: { a, b in a + b })
        return EditorNames(tables: tables, columns: columns)
    }

    /// The order the grid's header shows.
    var shownSort: ColumnSort? { shown == .table ? tableSort : querySort }

    /// A header click: the table again from its first page, or the query result reordered.
    func sort(by sort: ColumnSort) async {
        if shown == .table {
            tableSort = sort
            offset = 0
            await loadTable()
        } else {
            querySort = sort
        }
    }

    /// Rows of this table page can be edited: a table with a primary key or a ctid.
    var canEditRows: Bool { shown == .table && tableResult?.keys != nil && loadedRelation?.id == selectedRelation }
    /// New rows need a table, not a view.
    var canInsertRows: Bool {
        guard shown == .table, tableResult != nil, let loaded = loadedRelation, loaded.id == selectedRelation else { return false }
        if loaded.kind == .key { return ["hash", "list", "set", "zset"].contains(loaded.subtype) }
        return [.table, .partitioned].contains(loaded.kind)
    }

    /// The bottom panel runs Redis commands, one per line, instead of SQL.
    var usesCommands: Bool { session?.engine == .redis }
    /// The bottom panel runs mongosh-style queries (db.users.find({…})) instead of SQL.
    var usesQueries: Bool { session?.engine == .mongodb }

    /// What the bottom panel speaks, for its title and colors.
    var editorLanguage: EditorLanguage {
        switch session?.engine {
        case .redis: .redis
        case .mongodb: .mongo
        default: .sql
        }
    }

    /// What the editor starts with for each engine, so the panel is useful at once.
    static func starter(for engine: Engine) -> String {
        switch engine {
        case .postgres, .mysql: "select now();"
        case .sqlite: "select sqlite_version();"
        case .redis: "INFO server"
        case .mongodb: "show collections"
        }
    }

    /// Opens the database's own command-line client (psql, mysql, sqlite3, redis-cli,
    /// mongosh) in the default terminal, connected, through the app's SSH tunnel if any.
    func openInTerminal(_ connection: SavedConnection? = nil) {
        guard let connection = connection ?? activeConnection, let text = connection.url else { return }
        let tunnelPort = connection.uuid == activeID ? session?.tunnel?.localPort : nil
        if connection.ssh != nil, tunnelPort == nil {
            alert(String(localized: "Connect first"),
                  String(localized: "This connection goes through SSH. Connect to it in dbLens, then open the terminal: it uses the same tunnel."))
            return
        }
        do {
            let command = try ClientCommand.make(for: text, tunnelPort: tunnelPort)
            guard let tool = command.resolve() else {
                alert(String(localized: "\(command.tools[0]) isn't installed"),
                      String(localized: "Install it with Homebrew, then try again:\n\n\(command.install)"),
                      copy: command.install)
                return
            }
            // A script only this user can read; it deletes itself before starting the client.
            let file = FileManager.default.temporaryDirectory.appending(path: "dblens-\(UUID().uuidString).command")
            try writePrivately(Data(command.script(executable: tool).utf8), to: file)
            try FileManager.default.setAttributes([.posixPermissions: 0o700], ofItemAtPath: file.path)
            NSWorkspace.shared.open(file)
            addLog(sql: "", message: String(localized: "Opened \(URL(fileURLWithPath: tool).lastPathComponent) in the terminal"), isError: false)
        } catch {
            addLog(sql: "", message: error.localizedDescription, isError: true)
        }
    }

    private func alert(_ title: String, _ message: String, copy text: String? = nil) {
        let alert = NSAlert()
        alert.messageText = title
        alert.informativeText = message
        alert.addButton(withTitle: String(localized: "OK"))
        if text != nil { alert.addButton(withTitle: String(localized: "Copy Command")) }
        if alert.runModal() == .alertSecondButtonReturn, let text { copy(text) }
    }

    /// Deletes a Redis key (DEL), from the sidebar.
    func deleteKey(_ relation: Relation) async {
        guard let session, relation.kind == .key else { return }
        do {
            _ = try await session.execute("DEL \"\(relation.name.replacingOccurrences(of: "\\", with: "\\\\").replacingOccurrences(of: "\"", with: "\\\""))\"")
            addLog(sql: "DEL ?", message: String(localized: "Deleted \(relation.name)"), isError: false)
            if selectedRelation == relation.id { await showTable(nil) }
            await refreshRelations()
        } catch {
            addLog(sql: "DEL ?", message: error.localizedDescription, isError: true)
        }
    }

    // MARK: Connections

    /// Checks the URL, connects once to prove it works (through SSH when
    /// given), then saves it and keeps the session.
    @discardableResult
    func save(url text: String, name: String, ssh: SSHTunnel.Config? = nil,
              editing existing: SavedConnection? = nil) async throws -> SavedConnection {
        let engine = Engine.detect(text)
        guard let engine, engine.isAvailable else {
            throw PostgresError(message: engine.map { String(localized: "dbLens can't connect to \($0.name) yet. Support is on the way.") }
                                ?? String(localized: "This doesn't look like a database URL."))
        }
        let session = try await Connector.connect(text, ssh: ssh)
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        let connection = existing ?? SavedConnection(name: "")
        connection.name = trimmed.isEmpty ? Connector.summary(text)?.suggestedName ?? engine.name : trimmed
        connection.ssh = ssh
        connection.engine = engine.rawValue
        try Secrets.setURL(Connector.clean(text), for: connection.uuid)
        if existing == nil { context.insert(connection) }
        try? context.save()
        await open(session, for: connection)
        return connection
    }

    func connect(_ connection: SavedConnection) async {
        guard connectingID == nil, connection.uuid != activeID || session == nil else { return }
        guard let text = connection.url else {
            disconnect()
            activeID = connection.uuid
            connectError = Secrets.loadError ?? String(localized: "This connection's URL wasn't found. Edit the connection and paste the URL again.")
            return
        }
        connectingID = connection.uuid
        connectError = nil
        defer { connectingID = nil }
        do {
            let session = try await Connector.connect(text, ssh: connection.ssh)
            await open(session, for: connection)
        } catch {
            disconnect()
            activeID = connection.uuid
            connectError = error.localizedDescription
        }
    }

    private func open(_ session: any DatabaseSession, for connection: SavedConnection) async {
        disconnect()
        self.session = session
        activeID = connection.uuid
        sqlHistory = Self.loadHistory(for: connection.uuid)
        if sql.isEmpty || Engine.allCases.map(Self.starter(for:)).contains(sql) { sql = Self.starter(for: session.engine) }
        connection.lastUsed = .now
        try? context.save()
        watchTunnel(of: session, for: connection)
        let message = if let tunnel = session.tunnel {
            String(localized: "Connected to \(connection.name) via SSH \(tunnel.config.label) (\(session.engine.name) \(session.serverVersion))")
        } else {
            String(localized: "Connected to \(connection.name) (\(session.engine.name) \(session.serverVersion))")
        }
        addLog(sql: "", message: message, isError: false)
        await refreshRelations()
    }

    // MARK: SSH tunnel

    /// When the tunnel drops (network change, sleep, server restart), reconnect
    /// in place: same table, same page, same SQL.
    private func watchTunnel(of session: any DatabaseSession, for connection: SavedConnection) {
        let id = connection.uuid
        // Weak: the session owns the tunnel, which owns this closure.
        session.tunnel?.onUnexpectedExit = { [weak self, weak session] message in
            Task { @MainActor in
                guard let session else { return }
                await self?.tunnelDropped(message, connection: id, session: session)
            }
        }
    }

    private func tunnelDropped(_ message: String, connection id: UUID, session dropped: any DatabaseSession) async {
        guard activeID == id, session === dropped, let connection = activeConnection,
              let text = connection.url else { return }
        addLog(sql: "", message: String(localized: "The SSH tunnel dropped (\(message)). Reconnecting…"), isError: true)
        for (attempt, wait) in [1, 3, 8].enumerated() {
            try? await Task.sleep(for: .seconds(wait))
            guard activeID == id, session === dropped else { return }
            do {
                let fresh = try await Connector.connect(text, ssh: connection.ssh)
                guard activeID == id, session === dropped else { return }
                session = fresh
                watchTunnel(of: fresh, for: connection)
                addLog(sql: "", message: String(localized: "Reconnected via SSH \(fresh.tunnel?.config.label ?? "")"), isError: false)
                await refreshRelations()
                await loadTable()
                return
            } catch {
                if attempt == 2 {
                    disconnect()
                    activeID = id
                    connectError = String(localized: "The SSH tunnel dropped and did not come back: \(error.localizedDescription)")
                }
            }
        }
    }

    func disconnect() {
        session?.tunnel?.close()
        session = nil
        schemaGraph = nil
        schemaError = nil
        boardSchema = nil
        board = nil
        activeID = nil
        connectError = nil
        relations = []
        sqlHistory = []
        selectedRelation = nil
        tableResult = nil
        tableCount = nil
        tableError = nil
        execution = nil
        shown = .table
    }

    func delete(_ connection: SavedConnection) {
        if connection.uuid == activeID { disconnect() }
        Secrets.removeURL(for: connection.uuid)
        UserDefaults.standard.removeObject(forKey: "sqlHistory.\(connection.uuid.uuidString)")
        context.delete(connection)
        try? context.save()
    }

    // MARK: Tables

    func refreshRelations() async {
        guard let session else { return }
        do {
            relations = try await session.relations()
            if let selectedRelation, !relations.contains(where: { $0.id == selectedRelation }) { self.selectedRelation = nil }
        } catch {
            connectError = error.localizedDescription
        }
    }

    /// Shows a table's rows in the grid; picking the same table again just switches back to it.
    func showTable(_ id: Relation.ID?) async {
        shown = .table
        guard id != selectedRelation || tableResult == nil else { return }
        if id != selectedRelation { clearSearch() }
        selectedRelation = id
        offset = 0
        await loadTable()
    }

    func loadTable() async {
        guard let session, let relation else {
            tableResult = nil
            tableCount = nil
            tableError = nil
            return
        }
        tableLoading = true
        defer { tableLoading = false }
        let offset = offset
        do {
            let search = tableSearch
            let sort = tableSort
            let result = try await session.page(of: relation, limit: pageSize, offset: offset, search: search, sort: sort)
            // Ignore an answer for a table or page that is no longer shown.
            guard relation.id == selectedRelation, offset == self.offset, search == tableSearch, sort == tableSort else { return }
            if offset == 0 { tableCount = try? await session.count(of: relation, search: search) }
            // Rebuild the grid (and scroll to the top) only for another table,
            // page, or set of columns; an edit keeps the place.
            let shape = "\(relation.id)|\(offset)|\(search)|\(String(describing: sort))|\(result.columns.map(\.name))"
            if shape != tableShape || tableResult == nil { tableVersion += 1 }
            tableShape = shape
            tableResult = result
            loadedRelation = relation
            tableError = nil
        } catch {
            guard relation.id == selectedRelation else { return }
            tableResult = nil
            tableError = error.localizedDescription
        }
    }

    var canGoBack: Bool { shown == .table && offset > 0 }
    var canGoForward: Bool {
        guard shown == .table, let tableResult else { return false }
        if let tableCount { return offset + tableResult.rows.count < tableCount }
        return tableResult.rows.count == pageSize
    }

    /// A new page size starts again from the first page.
    func setPageSize(_ size: Int) async {
        guard size != pageSize else { return }
        pageSize = size
        offset = 0
        await loadTable()
    }

    /// Another table starts unfiltered, in key order.
    private func clearSearch() {
        searchTask?.cancel()
        tableSearch = ""
        tableSort = nil
    }

    /// Filters the table as you type, a moment after the last key.
    func searchTable(_ text: String) {
        tableSearch = text
        searchTask?.cancel()
        searchTask = Task {
            try? await Task.sleep(for: .milliseconds(300))
            guard !Task.isCancelled else { return }
            offset = 0
            await loadTable()
        }
    }

    func nextPage() async {
        guard canGoForward else { return }
        offset += pageSize
        await loadTable()
    }

    func previousPage() async {
        guard canGoBack else { return }
        offset = max(0, offset - pageSize)
        await loadTable()
    }

    /// Puts a starter query for the table in the SQL editor.
    func queryTable(_ relation: Relation) {
        if usesQueries {
            let plain = relation.name.range(of: "^[A-Za-z_][A-Za-z0-9_]*$", options: .regularExpression) != nil
            let collection = plain ? "db.\(relation.name)" : "db.getCollection(\"\(relation.name.replacingOccurrences(of: "\"", with: "\\\""))\")"
            sql = (relation.schema.isEmpty ? "" : "use \(relation.schema)\n") + "\(collection).find({}).limit(100)"
            sqlSelection = ""
            return
        }
        if relation.kind == .key {
            let key = "\"" + relation.name.replacingOccurrences(of: "\\", with: "\\\\").replacingOccurrences(of: "\"", with: "\\\"") + "\""
            sql = "TYPE \(key)\nTTL \(key)\nMEMORY USAGE \(key)"
        } else {
            sql = "select *\nfrom \(session?.qualified(relation) ?? relation.qualified)\nlimit 100;"
        }
        sqlSelection = ""
    }

    // MARK: Editing rows

    /// Saves the row editor: changed columns for an existing row, filled
    /// columns for a new one. Reloads the page and logs what ran.
    func saveRow(_ row: Int?, values: [(String, String?)]) async throws {
        guard let session else { return }
        guard let relation = loadedRelation, relation.id == selectedRelation else {
            throw PostgresError(message: String(localized: "Wait for the table to finish loading, then try again."))
        }
        let outcome: EditOutcome
        if let row {
            guard let keys = tableResult?.keys else { return }
            outcome = try await logged { try await session.update(relation, keys: keys, row: row, changes: values) }
        } else {
            outcome = try await logged { try await session.insert(relation, values: values) }
        }
        if !outcome.sql.isEmpty { addLog(sql: outcome.sql, message: outcome.command, isError: false) }
        if row == nil, offset == 0 { tableCount = tableCount.map { $0 + 1 } }
        await loadTable()
    }

    /// Saves one cell edited in the grid. Unchanged values send nothing.
    func updateCell(row: Int, column: Int, value: String?) async throws {
        guard let result = tableResult, column < result.columns.count, row < result.rows.count,
              result.rows[row][column] != value else { return }
        try await saveRow(row, values: [(result.columns[column].name, value)])
    }

    func deleteRow(_ row: Int) async throws {
        guard let session, let keys = tableResult?.keys else { return }
        guard let relation = loadedRelation, relation.id == selectedRelation else {
            throw PostgresError(message: String(localized: "Wait for the table to finish loading, then try again."))
        }
        let outcome = try await logged { try await session.delete(relation, keys: keys, row: row) }
        addLog(sql: outcome.sql, message: outcome.command, isError: false)
        tableCount = tableCount.map { max(0, $0 - outcome.affected) }
        await loadTable()
    }

    /// Runs an edit and logs its error before passing it on to the editor.
    private func logged(_ edit: () async throws -> EditOutcome) async throws -> EditOutcome {
        do {
            return try await edit()
        } catch {
            addLog(sql: "", message: error.localizedDescription, isError: true)
            throw error
        }
    }

    // MARK: SQL

    func run() async {
        guard let session, !running else { return }
        let picked = sqlSelection.trimmingCharacters(in: .whitespacesAndNewlines)
        let text = (picked.isEmpty ? sql : picked).trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return }
        showSQLPanel = true
        running = true
        defer { running = false }
        do {
            let execution = try await session.execute(text)
            self.execution = execution
            querySort = nil
            remember(text, failed: false)
            runVersion += 1
            shown = .query
            addLog(sql: text, message: Self.summary(execution), isError: false)
            // A CREATE, ALTER, or DROP changes the table list; any write may change the rows.
            // Redis commands can add or remove keys anywhere: list them again after every run.
            if usesCommands || usesQueries || execution.results.contains(where: { ["CREATE", "DROP", "ALTER"].contains(where: $0.command.hasPrefix) }) {
                await refreshRelations()
            }
            if execution.results.contains(where: { !$0.hasRows }) { await loadTable() }
        } catch {
            remember(text, failed: true)
            addLog(sql: text, message: error.localizedDescription, isError: true)
        }
    }

    // MARK: History

    /// Puts a statement on top of the history (once: running it again moves it up).
    private func remember(_ text: String, failed: Bool) {
        sqlHistory.removeAll { $0.sql == text }
        sqlHistory.insert(HistoryEntry(sql: text, failed: failed), at: 0)
        if sqlHistory.count > 100 { sqlHistory.removeLast(sqlHistory.count - 100) }
        saveHistory()
    }

    func clearHistory() {
        sqlHistory = []
        saveHistory()
    }

    /// From the History list: into the editor, and run it when asked.
    func useHistory(_ entry: HistoryEntry, run: Bool) async {
        sql = entry.sql
        sqlSelection = ""
        if run { await self.run() }
    }

    private func saveHistory() {
        guard let activeID, !Store.isOverridden, let data = try? JSONEncoder().encode(sqlHistory) else { return }
        UserDefaults.standard.set(data, forKey: "sqlHistory.\(activeID.uuidString)")
    }

    private static func loadHistory(for id: UUID) -> [HistoryEntry] {
        guard !Store.isOverridden, let data = UserDefaults.standard.data(forKey: "sqlHistory.\(id.uuidString)") else { return [] }
        return (try? JSONDecoder().decode([HistoryEntry].self, from: data)) ?? []
    }

    func cancel() {
        session?.cancel()
    }

    func refresh() async {
        await refreshRelations()
        if shown == .schema { await loadSchema() } else { await loadTable() }
    }

    // MARK: Back and Forward

    /// Where the window is: the connection, then the table and its page, a query
    /// result, or the board and its schema. Back and Forward walk these, across connections.
    struct Place: Equatable {
        var connection: UUID?
        var shown: Shown
        var relation: Relation.ID?
        var offset = 0
        var run = 0
        var execution: Execution?
        var boardSchema: String?

        /// Nothing open: not a page worth going back to.
        var isEmpty: Bool { connection == nil || (shown == .table && relation == nil) }

        static func == (a: Place, b: Place) -> Bool {
            a.connection == b.connection && a.shown == b.shown && a.relation == b.relation
                && a.offset == b.offset && a.run == b.run && a.boardSchema == b.boardSchema
        }
    }

    private(set) var backStack: [Place] = []
    private(set) var forwardStack: [Place] = []
    @ObservationIgnored private var currentPlace: Place?
    @ObservationIgnored private var travelling = false
    @ObservationIgnored private var movePending = false

    private var place: Place {
        switch shown {
        case .table: Place(connection: activeID, shown: .table, relation: selectedRelation, offset: offset)
        case .query: Place(connection: activeID, shown: .query, run: runVersion, execution: execution)
        case .schema: Place(connection: activeID, shown: .schema, boardSchema: boardSchema)
        }
    }

    var canGoBackInHistory: Bool { !backStack.isEmpty && connectingID == nil }
    var canGoForwardInHistory: Bool { !forwardStack.isEmpty && connectingID == nil }

    /// Records a move once the current change settles, so one action that
    /// changes several things (a table, then its page) counts as one step.
    private func noteMove() {
        guard !travelling, !movePending else { return }
        movePending = true
        Task { @MainActor in
            movePending = false
            let now = place
            guard !travelling, now != currentPlace, !now.isEmpty else { return }
            if let last = currentPlace, !last.isEmpty {
                backStack.append(last)
                if backStack.count > 50 { backStack.removeFirst() }
            }
            forwardStack = []
            currentPlace = now
        }
    }

    func goBack() async {
        while let target = backStack.popLast() {
            let here = place
            if await travel(to: target) {
                if !here.isEmpty { forwardStack.append(here) }
                return
            }
        }
    }

    func goForward() async {
        while let target = forwardStack.popLast() {
            let here = place
            if await travel(to: target) {
                if !here.isEmpty { backStack.append(here) }
                return
            }
        }
    }

    /// Goes to a place, reconnecting when it belongs to another connection.
    /// False when it no longer exists (a deleted connection or table).
    private func travel(to target: Place) async -> Bool {
        travelling = true
        defer { travelling = false; currentPlace = place }
        if target.connection != activeID || session == nil {
            guard let id = target.connection,
                  let saved = try? context.fetch(FetchDescriptor<SavedConnection>(predicate: #Predicate { $0.uuid == id })).first
            else { return false }
            await connect(saved)
            guard activeID == id, session != nil else { return true }
        }
        switch target.shown {
        case .table:
            guard let id = target.relation, relations.contains(where: { $0.id == id }) else { return false }
            shown = .table
            if id != selectedRelation { clearSearch() }
            selectedRelation = id
            offset = target.offset
            await loadTable()
        case .query:
            if let saved = target.execution { execution = saved }
            guard execution != nil else { return false }
            shown = .query
        case .schema:
            guard hasSchemaBoard else { return false }
            boardSchema = target.boardSchema
            await showSchema()
        }
        return true
    }

    // MARK: Schema board

    private(set) var schemaGraph: SchemaGraph?
    private(set) var schemaError: String?
    private(set) var schemaLoading = false
    /// The schema the board shows; nil shows all.
    var boardSchema: String? { didSet { noteMove() } }
    /// The board's cards, notes, colors, and view, saved per connection.
    private(set) var board: BoardState?

    /// The schema board needs tables and foreign keys, which Redis doesn't have.
    var hasSchemaBoard: Bool { session != nil && session?.engine != .redis }

    func showSchema() async {
        shown = .schema
        if schemaGraph == nil { await loadSchema() }
    }

    func loadSchema() async {
        guard let session else { return }
        schemaLoading = true
        defer { schemaLoading = false }
        do {
            let graph = try await session.schemaGraph()
            schemaGraph = graph
            schemaError = nil
            if board?.connection != activeID, let activeID { board = BoardState(connection: activeID) }
            board?.place(graph.tables, links: graph.links)
            // All schemas by default, so relations across schemas show.
            if let schema = boardSchema, !graph.schemas.contains(schema) { boardSchema = nil }
        } catch {
            schemaError = error.localizedDescription
        }
    }

    /// Fits every visible card and note in the board's view (Command-0).
    func fitBoard() {
        guard let board, let graph = schemaGraph else { return }
        let tables = boardSchema.map { schema in graph.tables.filter { $0.schema == schema } } ?? graph.tables
        board.fit(BoardLayout.bounds(tables, notes: board.notes, board: board))
    }

    /// Double-click on a card: its rows, in the grid.
    func openFromBoard(_ table: SchemaGraph.Table) async {
        guard let relation = relations.first(where: { $0.schema == table.schema && $0.name == table.name }) else { return }
        await showTable(relation.id)
    }

    private func addLog(sql: String, message: String, isError: Bool) {
        lastLog = LogEntry(sql: sql, message: message, isError: isError)
    }

    /// "3 linhas · 12 ms", "UPDATE 2 · 4 ms", with the statement count when more than one.
    static func summary(_ execution: Execution) -> String {
        var parts: [String] = []
        if let main = execution.main {
            if main.hasRows {
                let total = main.totalRows.formatted()
                if main.truncated {
                    parts.append(String(localized: "\(main.rows.count.formatted()) of \(total) rows (truncated)"))
                } else {
                    parts.append(main.totalRows == 1 ? String(localized: "\(total) row") : String(localized: "\(total) rows"))
                }
            } else if !main.command.isEmpty {
                parts.append(main.command)
            }
        }
        if execution.results.count > 1 { parts.append(String(localized: "\(execution.results.count) commands")) }
        let ms = Double(execution.duration.components.attoseconds) / 1e15 + Double(execution.duration.components.seconds) * 1000
        parts.append(ms < 1000 ? "\(Int(ms.rounded())) ms" : String(format: "%.2f s", ms / 1000))
        return parts.joined(separator: " · ")
    }
}
